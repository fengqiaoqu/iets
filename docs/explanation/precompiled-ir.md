# What precompiled nixpkgs IR would buy

Suppose nixpkgs shipped its IR already compiled. Compilation then costs
nothing at evaluation time, so any analysis is affordable, however slow.
This page measures what that buys. The short answer: not enough to build.
The warm IR cache already removes most of the compile cost. Two
whole-program analyses with an unlimited budget change the run by less
than 0.1 % and at most 2 %.

All numbers are from 2026-10-19, on a quiet 32-core machine, with iets at
`bdd26f2c`, the nixpkgs of `flake.lock` (`7fd4004d`) and
`--no-tracing-cache`. Every drvPath matched `nix-instantiate`.

## The ceiling

The IR cache ([How caching works](caching.md)) already compiles each file
once. So precompiled IR can save at most two things: the compile on a first
run, and the load on a warm run. `bench/aot/ceiling.sh` measures both:

```
bench/aot/ceiling.sh ./target/release/iets /tmp/aot-out
```

Means of 10 runs (3 for outpaths, chunk 0 of 8 with `eval-jobs`):

| workload | workers | no IR cache | first run, fills the cache | warm | warm saves |
|---|---|---|---|---|---|
| `hello.drvPath` | 1 | 63.7 ms | 80.6 ms | 55.3 ms | 13 % |
| `hello.drvPath` | 4 | 64.4 ms | 86.2 ms | 56.9 ms | 12 % |
| nixos-server | 1 | 1,039 ms | 1,225 ms | 887 ms | 14.6 % |
| nixos-server | 4 | 573 ms | 826 ms | 471 ms | 17.8 % |
| outpaths 0/8 | 1 | 22.16 s | | 21.42 s | 3.4 % |
| outpaths 0/8 | 4 | 8.39 s | | 8.10 s | 3.4 % |

A warm run still loads every entry. `IETS_IR_STATS=1` times each load,
which includes reading and hashing the source for the key:

| workload, 1 worker | files loaded | sum of load times | share of the warm run |
|---|---|---|---|
| nixos-server | 2,860 (38 MB of source) | 116–122 ms | 13 % |
| outpaths 0/8 | 27,168 | 712 ms | 3.3 % |

So against today's warm cache, shipped IR saves at most the load: 13 % of
a NixOS configuration and 3.3 % of a nixpkgs-wide evaluation, at one
worker. That bound assumes a load that costs zero. An `mmap` still faults
the pages in and still needs a symbol for every name. On a first run, it
saves the 338 ms between the first and the warm nixos-server run, 28 % of
the first run, once per machine and per IR version.

In a `perf` profile of the warm nixos-server run, the decoder
(`De::node`, `De::binds`, `De::sym`) takes 3.3 % of the samples, symbol
interning 2.6 % and blake3 1.3 %. A precomputed symbol table and a
pointer-free IR format would remove most of that. Both change how every
IR node is read, and the prize is under the 13 % above.

## The analyses

The rest of the prize has to come from analysis that a per-file compiler
cannot afford. The census patch `bench/aot/census.patch` counts what each
one could remove. It applies to `bdd26f2c` and is not part of the build.

| analysis | what was measured | effect | verdict |
|---|---|---|---|
| resolve `with` | every `WithVar` evaluation in nixos-server | under 0.1 % | not worth it |
| evaluate always-forced thunks directly | a profile oracle, timed against a control | 2.0 % faster at most, one worker only | not worth it |
| `mmap`-able IR, precomputed symbols | the load time above | at most 13 % warm, 3.3 % nixpkgs-wide | only if the load becomes the bottleneck |
| specialise `callPackage`, fold `lib` | the evaluation profile | not priced; argued below | not worth it |

### Resolving `with`

`with` is the one construct the compiler leaves to run time. It looked
like the biggest single win. It is the smallest. nixos-server evaluates
20,030 `WithVar` nodes in the whole run:

| | count |
|---|---|
| found in the innermost `with` | 18,849 (94.1 %) |
| found in the second | 1,171 |
| found in the third | 10 |
| fell through to a global | 0 |
| lookups that missed a scope | 1,191 |

A resolved variable still forces the scope and looks the name up, so
resolution can only remove the 1,191 missed lookups and a short walk up
the environment. That is well under a millisecond of an 887 ms run. Of
the 15,178 `WithVar` thunks that were made, 81 % were never forced.

### Strictness

nixos-server makes 6.63 million thunks, and forces 61 % of them. At sites
where the environment is complete (an argument, a list element, a plain
attribute), 2.05 million thunks come from sites that were forced every
time. The upper bound for a strictness analysis is an oracle: run once,
record those sites, then evaluate them directly on the next run. That is
more than any sound analysis can prove.

| run, 1 worker | thunks made | wall | its control | max RSS, against control |
|---|---|---|---|---|
| oracle, every always-forced site | 4.59 M | 1,055 ms | 1,002 ms | |
| oracle, without the sites that failed when forced early | 4.85 M | 990 ms | 1,011 ms | 707 against 723 MB |

The control is the same binary doing the same site lookup with no site
matching, so it pays the cost of the experiment and gains nothing. It makes
the 6.63 M thunks of the base run. Each oracle row was timed with its own
build of the patch.

The first oracle is slower. 102,314 early evaluations failed, so their
work was done twice. It also printed a warning that Nix never prints, from
an option documentation path that this run happened to force. With the
failing sites removed, the oracle saves 27 % of the thunks and 2.0 % of
the wall, and 2.2 % of the peak RSS. A thunk is two words from a bump
arena, so it costs little to make.

The oracle also hangs at four workers. An early evaluation is not a
thunk, so the deadlock breaker in [How iets evaluates](evaluation.md) has
nothing to reset. A real analysis would need that fixed first.

### `callPackage` and `lib`

These were not built, because the profile gives no room for them. In the
evaluation profile of nixos-server, `«__functionArgs»` is the leaf of
6.4 % of the samples. That time is the `import` that `callPackage` forces,
and the load of that file has no frame of its own, so it is the load cost
above, counted again. Lambdas from `lib/` are the leaf of 35 % of the
samples. Folding needs a value that does not depend on the
configuration, and such a value in `lib` is already a shared thunk, forced
once per run.

## Versioning precompiled IR

Precompiled IR is valid only for a compiler that produces the same IR and
an evaluator that gives each node the same meaning. Today the cache key
holds the build id, so any new binary misses. That is safe, but it throws
away every entry on a patch release.

Shipped IR would need two numbers in the source, each bumped by hand:

- **IR format**: the node set and the encoding. A test would fail when
  `compile/ir.rs` or the encoder changes and the number does not.
- **IR semantics**: what a node means and what the compiler folds. A
  change to a fold, to scope resolution, or a new entry in
  [Known divergences](../reference/divergences.md) that the compiler
  decides, bumps it.

The key is then those two numbers, the file's content hash, and the
configuration the compiler folds (system, store directory, `pure-eval`,
lint levels), as the IR cache key has today. A patch release keeps every
entry. A semantics change misses every entry, so it cannot poison one. The
decoder already treats a crafted entry as a miss, so a wrong number can
give a wrong value but never a crash. To catch a missed bump, the job that
publishes the IR also compares the drvPaths of a run from the IR against
a run from source.

The entries belong in the binary cache, keyed per file. One nixpkgs
revision changes few files, so a per-file key lets revisions share
entries, as the local cache already does (56 entries, 0.5 MiB, for a
nixpkgs two releases older). An entry beside nixpkgs in git ties nixpkgs to
one evaluator. An entry in the input store is the local cache under
another name.

## Recommendation

Don't build it. The prize is the load on a warm run, at most 13 % of a
NixOS configuration and 3.3 % of nixpkgs. The whole-program analyses that
the offline budget makes affordable measured under 0.1 % and at most 2 %,
and the second one breaks parallel evaluation.

One part is worth taking on its own. Replace the build id in the IR cache
key with the format and semantics numbers above. Then a patch release
keeps the warm cache, and users skip the 28 % first-run cost on nixos-server
after each upgrade.
