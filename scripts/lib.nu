# Helpers shared by the scripts in this directory.

# Strip trailing newlines, like bash's `$(...)`.
export def chomp []: string -> string { str trim -r -c "\n" }

# Print a list of lines; nothing for an empty list.
export def show []: list<string> -> nothing { each {|l| print $l } | ignore }

# The first line of NAME.flags split on whitespace, or [].
export def flags-of [n: string]: nothing -> list<string> {
  let f = $"($n).flags"
  if not ($f | path exists) { return [] }
  open --raw $f | lines | get -o 0 | default "" | str trim | split row -r '\s+' | where $it != ""
}

# `diff A B | head -N`. The `let` keeps diff's exit code 1 from raising.
export def diff-head [a: string, b: string, n: int]: nothing -> list<string> {
  let d = (^diff $a $b | lines | first $n)
  $d
}

# $NAME split on spaces, so NIX="nix eval" style values keep working.
export def env-cmd [name: string, default: string]: nothing -> list<string> {
  $env | get -o $name | default $default | split row " " | where $it != ""
}

# Files matching PAT in bash glob order (strcoll of the current locale).
export def glob-sorted [pat: string]: nothing -> list<string> {
  let names = (ls ($pat | into glob) | get name | str join "\n" | ^sort | lines)
  $names
}

# The repository root, whatever directory a script runs from.
export const ROOT = (path self | path dirname | path dirname)

# The nixpkgs tree this repo pins; $NIXPKGS overrides it.
export def nixpkgs []: nothing -> string {
  $env.NIXPKGS? | default "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source"
}

# The iets under test, as an absolute path: $IETS, else $BIN/iets, else
# target/release/iets.
export def iets-bin []: nothing -> string {
  let p = if ($env.IETS? | is-not-empty) {
    $env.IETS | path expand
  } else if ($env.BIN? | is-not-empty) {
    $env.BIN | path join iets | path expand
  } else {
    $ROOT | path join target/release/iets
  }
  if not ($p | path exists) { error make -u {msg: $"no ($p); cargo build --release first"} }
  $p
}

export def current-system []: nothing -> string {
  ^nix-instantiate --eval -E builtins.currentSystem | str replace -a '"' '' | chomp
}

# Every e2e script calls this first. It returns DIR, or a fresh temporary
# directory named after NAME, and points iets's config lookup into it: the
# host's iets.toml must not steer the runs (a remote `builder` changes the
# cores, the out-links and where things build).
export def --env isolate [name: string, dir?: string]: nothing -> string {
  $env.TMPDIR = ($env.TMPDIR? | default "/tmp")
  $env.NIX_REMOTE = ($env.NIX_REMOTE? | default "daemon")
  let work = ($dir | default (mktemp -d --tmpdir $"iets-($name).XXXXXX"))
  $env.XDG_CONFIG_HOME = ($work | path join no-xdg)
  hide-env -i IETS_CONFIG
  $work
}

# Runs BODY, then kills the jobs it spawned, removes WORK and exits with
# BODY's code: its int result, 0 for any other result, 1 on an error. `fail`
# already printed its message; any other error is printed here.
export def run-in [work: string, body: closure] {
  let code = (try {
    let r = (do $body)
    if ($r | describe) == "int" { $r } else { 0 }
  } catch {|e|
    if $e.msg != "fail" { print -e $e.rendered }
    1
  })
  for j in (job list | get id) { job kill $j }
  cd /
  # A scratch store's paths are read-only, as in a real store.
  ^chmod -R u+w $work err> /dev/null | complete | ignore
  rm -rf $work
  exit $code
}

# Like bash `set -e`: an error that unwinds to `run-in`, which exits 1.
export def fail [msg: string] {
  print -e $"FAIL: ($msg)"
  error make -u {msg: "fail"}
}

# `open --raw` is a byte stream; == against a string is always false, so compare bytes.
export def bytes-of [f: string]: nothing -> binary { open --raw $f | into binary }
export def same-file [a: string, b: string]: nothing -> bool { (bytes-of $a) == (bytes-of $b) }

# The Nix release every comparison in this repo is written against
# (docs/reference/divergences.md).
const REF_NIX = "2.34.8"

# CMD with every symlink resolved, or "" when there is none. CMD is a name to
# look up on PATH or a path of its own; `command -v` takes both.
export def resolved-cmd [cmd: string]: nothing -> string {
  let p = (^bash -c $"command -v -- '($cmd)' || true" | chomp)
  if ($p | is-empty) { "" } else { ^readlink -f $p | chomp }
}

# Aborts when one of CMDS (default `nix` and `nix-instantiate`) is iets under
# a Nix command name, so a script that compares against the real nix cannot
# silently compare iets with itself. The test is the one iets's own fallback
# uses on the same path (`is_iets` in src/nixbin.rs): a farm entry is a
# symlink chain that ends in a file named `iets`. Nothing behavioural can
# tell them apart, because the farm execs the real nix for whatever it does
# not implement, `--version` included. A missing command is not an error here;
# the script fails when it runs it. Call it before the first temporary
# directory: it exits the process.
export def require-real-nix [...cmds: string] {
  let want = (if ($cmds | is-empty) { [nix nix-instantiate] } else { $cmds })
  for c in $want {
    let real = (resolved-cmd $c)
    if ($real | path basename) == "iets" {
      print -e $"FAIL: `($c)` is iets, not Nix: ($real)"
      print -e "  This script compares against the real nix, so it would compare iets with itself."
      print -e "  Run it inside the dev shell: nix develop -c nu scripts/NAME.nu"
      print -e "  IETS_NIX_BIN_DIR does not help: the reference side runs what PATH has."
      exit 1
    }
  }
  warn-nix-version ($want | first)
}

# A warning when the reference nix is not the release the comparisons were
# written against. Another release gives confusing diffs, not wrong ones, so
# this never fails.
def warn-nix-version [cmd: string] {
  let real = (resolved-cmd $cmd)
  if ($real | is-empty) { return }
  let v = (^$real --version | complete | get stdout | parse -r '\(Nix\) (?<v>\S+)' | get -o v.0)
  if $v != null and $v != $REF_NIX {
    print -e $"warning: comparing against nix ($v), not ($REF_NIX); some diffs may be version drift"
  }
}
