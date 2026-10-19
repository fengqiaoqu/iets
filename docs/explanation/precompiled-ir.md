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

### Bundles

A few large bundles instead of one entry per file would remove system
calls. A warm nixos-server run at one worker makes these, counted with
`perf trace -s`:

| call | count | time under the tracer |
|---|---|---|
| `statx` | 34,627 | 69 ms |
| `read` | 17,441 | 34 ms |
| `openat` | 11,521 | 34 ms |
| `close` | 11,513 | 18 ms |
| `getdents64` | 2,225 | 10 ms |

The tracer adds its own cost to each call. Without it, the whole run spends
110 ms in the kernel, 12 % of the wall, so the file calls cost less than
that. A bundle removes the 2,863 entry opens and the source reads for the
key. Most of the rest are repeats: 21,365 `statx` calls on nixpkgs name
only 4,220 paths, and 8,639 opens name 5,896. A store path never changes,
so iets can remember its `stat` within one run, with no bundle.

A bundle does not make the download smaller. The 20,909 files that outpaths
chunk 0 loads are 87.6 MB of source and 63.1 MB of IR. Compressed, the IR
is larger than the source:

| | tar | zstd -19 | xz -9 |
|---|---|---|---|
| IR of the 20,909 files outpaths loads | 79 MB | 25.9 MB | 22.6 MB |
| every `.nix` file of nixpkgs, 45,003 files | 151 MB | 20.9 MB | 20.2 MB |
| all of nixpkgs | 255 MB | 38.8 MB | 36.4 MB |

The IR also can't replace the source. Builders, patches and the `.nix`
files that a derivation copies as a `src` still come from the tree, and an
error message prints the source line. So IR is an addition to the download,
about 26 MB on top of 39 MB.

## The analyses

The rest of the prize has to come from analysis that a per-file compiler
cannot afford. The census patch `bench/aot/census.patch` counts what each
one could remove. It applies to `bdd26f2c` and is not part of the build.

| analysis | what was measured | effect | verdict |
|---|---|---|---|
| resolve `with` | every `WithVar` evaluation in nixos-server | under 0.1 % | not worth it |
| evaluate always-forced thunks directly | a profile oracle, timed against a control | 2.0 % faster at most, one worker only | not worth it |
| `mmap`-able IR, precomputed symbols | the load time above | at most 13 % warm, 3.3 % nixpkgs-wide | only if the load becomes the bottleneck |
| merge duplicate functions | the shape of every loaded IR tree | 0.5 % of the IR, no time | not worth it |
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
the environment. That is well under a millisecond of an 887 ms run. Outpaths
chunk 0 of 8 at one worker gives the same picture: 280,090 `WithVar`
evaluations, 96.3 % in the innermost `with`, and 10,330 missed lookups in a
21 s run. Of
the 15,178 `WithVar` thunks that were made, 81 % were never forced.

### Strictness

nixos-server makes 6.63 million thunks, and forces 61 % of them. At sites
where the environment is complete (an argument, a list element, a plain
attribute), 2.05 million thunks come from sites that were forced every
time. The upper bound for a strictness analysis is an oracle: run once,
record those sites, then evaluate them directly on the next run. That is
more than any sound analysis can prove.

| workload, 1 worker | thunks made | forced | from always-forced sites | of those, at eligible sites |
|---|---|---|---|---|
| nixos-server | 6.63 M | 61 % | 2.70 M (41 %) | 2.05 M |
| outpaths 0/8 | 91.4 M | 58 % | 33.2 M (36 %) | 18.8 M |

Outpaths has a smaller share of always-forced thunks, so its oracle would
gain no more than the nixos-server oracle below.

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

### Merging duplicate functions

A whole-program compiler could give identical functions one body. The
census hashes the shape of every loaded IR tree, with positions and names
left out. Because variables are slot numbers, two functions that differ
only in variable names hash the same.

| workload, 1 worker | IR nodes | functions | functions that repeat an earlier one | nodes in them | distinct subtrees |
|---|---|---|---|---|---|
| nixos-server | 1.56 M | 36,748 | 1,571 (4.3 %) | 7,736 (0.5 %) | 583,280 (37 %) |
| outpaths 0/8, largest worker | 3.03 M | 69,934 | 2,379 (3.4 %) | 12,332 (0.4 %) | 1.21 M (40 %) |

Most of the repeats are small closed functions such as `x: x`. Merging
whole functions saves 0.5 % of the IR, about 250 kB, and no time: every
evaluation of a function expression still makes its own closure. Sharing
such a closure was priced on [What was tried and removed](history.md) at
16 MB of a 7.1 GB arena.

Sharing every repeated subtree would cut the nodes by about 60 %. The IR
is 99 MiB of the 754 MiB peak RSS on nixos-server, so the bound is about
60 MiB of memory and no run time. Every node carries the position that an
error message prints, so shared nodes need their positions kept per use,
which takes back part of that memory.

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

A second part needs no shipping either. Remember the `stat` of a store path
for the run. That removes most of the 21,365 `statx` calls a bundle would
have removed.
