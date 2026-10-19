//! iets under Nix's command names against the real Nix, on one [`Store`].

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

pub use super::{Checks, records};
use super::{Store, resolved, stderr, stdout};

pub const NAMES: [&str; 5] = [
    "nix-instantiate",
    "nix-build",
    "nix-eval-jobs",
    "nix-shell",
    "nix",
];

/// The classic farm, as `nix build .#classic` lays it out: iets under Nix's
/// names, and a marker naming the real Nix it falls back to.
pub struct Farm<'a> {
    pub s: &'a Store,
    pub bin: PathBuf,
    pub real: PathBuf,
}

pub struct Both {
    pub ours: Output,
    pub real: Output,
}

impl<'a> Farm<'a> {
    pub fn new(s: &'a Store) -> Farm<'a> {
        let dir = s.dir().join("farm");
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        for n in NAMES {
            symlink(env!("CARGO_BIN_EXE_iets"), bin.join(n)).unwrap();
        }
        let real = resolved("nix-instantiate").parent().unwrap().to_path_buf();
        fs::create_dir_all(dir.join("share/iets")).unwrap();
        fs::write(dir.join("share/iets/real-nix-bin"), real.to_str().unwrap()).unwrap();
        Farm { s, bin, real }
    }

    /// iets as `name`.
    pub fn ours(&self, name: &str) -> Command {
        let mut c = self.s.iets_as(self.bin.join(name));
        // the fallback must reach the daemon, not open the store itself
        c.env("IETS_NIX_BIN_DIR", &self.real)
            .env("NIX_REMOTE", "daemon");
        c
    }

    /// The real `name`.
    pub fn real(&self, name: &str) -> Command {
        let mut c = self.s.nix(self.real.join(name).to_str().unwrap());
        // a shell's environment shows it, so both sides have it
        c.env("IETS_NIX_BIN_DIR", &self.real);
        c
    }

    pub fn both(&self, name: &str, args: &[&str]) -> Both {
        self.both_with(name, args, |_| {})
    }

    /// Both runs of `name args`, each changed by `f` first.
    pub fn both_with(&self, name: &str, args: &[&str], f: impl Fn(&mut Command)) -> Both {
        let run = |mut c: Command| {
            f(c.args(args));
            c.output().unwrap()
        };
        Both {
            ours: online(run(self.ours(name))),
            real: online(run(self.real(name))),
        }
    }

    /// The stderr of `name args` with IETS_NO_FALLBACK: why it would fall back.
    pub fn reason(&self, name: &str, args: &[&str]) -> String {
        stderr(
            &self
                .ours(name)
                .env("IETS_NO_FALLBACK", "1")
                .args(args)
                .output()
                .unwrap(),
        )
    }
}

/// `o` without the warning Nix prints when it finds no network, as in the
/// build sandbox.
pub fn online(mut o: Output) -> Output {
    const W: &[u8] =
        b"warning: you don't have Internet access; disabling some network-dependent features\n";
    if let Some(i) = o.stderr.windows(W.len()).position(|w| w == W) {
        o.stderr.drain(i..i + W.len());
    }
    o
}

impl Checks {
    /// The same stdout bytes and exit code.
    pub fn same(&mut self, label: &str, b: &Both) -> bool {
        let ok = b.ours.stdout == b.real.stdout && b.ours.status.code() == b.real.status.code();
        self.check(label, ok, show(b))
    }

    /// The same error record and exit code as Nix, and iets's own `code`.
    pub fn same_record(&mut self, label: &str, code: &str, b: &Both) {
        let (real, _) = err_record(&stderr(&b.real));
        let (ours, oc) = err_record(&stderr(&b.ours));
        self.check(
            label,
            real == ours
                && oc == code
                && b.ours.status.code() == b.real.status.code()
                && real["raw_msg"] != "",
            format!("real {real}\n  ours {ours} {oc}"),
        );
    }
}

/// The first level-0 error record, with the fields both sides fill alike.
pub fn err_record(stderr: &str) -> (Value, Value) {
    let r = records(stderr)
        .into_iter()
        .find(|r| r["level"] == 0 && r.get("raw_msg").is_some())
        .unwrap_or(Value::Null);
    let msg = strip_ansi(r["raw_msg"].as_str().unwrap_or(""));
    (
        serde_json::json!({"raw_msg": msg, "line": r["line"], "column": r["column"], "file": r["file"]}),
        r["code"].clone(),
    )
}

pub fn show(b: &Both) -> String {
    let cut = |s: String| s.chars().take(600).collect::<String>();
    format!(
        "exit ours={:?} real={:?}\n  ours stdout: {}\n  real stdout: {}\n  ours stderr: {}\n  real stderr: {}",
        b.ours.status.code(),
        b.real.status.code(),
        cut(stdout(&b.ours)),
        cut(stdout(&b.real)),
        cut(stderr(&b.ours)),
        cut(stderr(&b.real)),
    )
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' && it.peek() == Some(&'[') {
            for d in it.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The first `error:` line, without colours.
pub fn first_error(s: &str) -> String {
    strip_ansi(s)
        .lines()
        .find(|l| l.starts_with("error:"))
        .unwrap_or_default()
        .to_string()
}

/// The `env` lines of a shell both sides fill alike, sorted.
pub fn env_lines(text: &[u8]) -> Vec<String> {
    const SKIP: [&str; 12] = [
        "NIX_BUILD_TOP",
        "TMPDIR",
        "TMP",
        "TEMP",
        "TEMPDIR",
        "NIX_ATTRS_SH_FILE",
        "NIX_ATTRS_JSON_FILE",
        "SHLVL",
        "_",
        "PWD",
        "OLDPWD",
        "PS1",
    ];
    let mut v: Vec<String> = String::from_utf8_lossy(text)
        .lines()
        .filter(|l| {
            let k = l.split('=').next().unwrap_or("");
            l.contains('=') && !k.starts_with("__") && !SKIP.contains(&k)
        })
        .map(String::from)
        .collect();
    v.sort();
    v
}

/// The symlink at `p`, or "".
pub fn link(p: &Path) -> String {
    fs::read_link(p)
        .map(|t| t.to_string_lossy().into_owned())
        .unwrap_or_default()
}
