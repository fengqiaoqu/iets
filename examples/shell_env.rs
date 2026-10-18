//! Print the environment of a shell derivation as JSON, the way
//! `nix print-dev-env --json` does, for an expression rather than a flake.
//!
//! `cargo run --example shell_env -- [--gc-root PATH] [-A attr] (-E EXPR | FILE)`
//!
//! Uses the library only (`iets::api`), never `iets::cli`.

use iets::api::shell_env::{GcRoot, Options, Transport, shell_env};
use iets::api::{EvalArgs, Input};
use iets::eval::EvalConfig;
use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{self, ExitCode};

fn main() -> ExitCode {
    let session = iets::session::Session::new();
    session.log.write_to_stderr();
    let _entered = session.enter();
    let mut a = EvalArgs::new();
    let mut gc_root = GcRoot::None;
    let mut argv = env::args().skip(1);
    while let Some(arg) = argv.next() {
        let mut next = || {
            argv.next()
                .unwrap_or_else(|| die(&format!("{arg} needs a value")))
        };
        match arg.as_str() {
            "-E" | "--expr" => a.input = Some(Input::Expr(next())),
            "-A" | "--attr" => a.attr = Some(next()),
            "--gc-root" => gc_root = GcRoot::Given(PathBuf::from(next())),
            f if f.starts_with('-') => die(&format!("unknown flag {f}")),
            f => a.input = Some(Input::File(f.to_string())),
        }
    }
    if a.input.is_none() {
        die("give -E EXPR or a FILE");
    }
    let Some(transport) = Transport::parse("daemon") else {
        die("no local daemon");
    };
    let Ok(cfg) = a.config(EvalConfig::new().pure(false)) else {
        return ExitCode::from(1);
    };
    match shell_env(cfg, a, transport, Options::default(), gc_root) {
        Ok(env) => {
            let mut out = serde_json::to_vec(&env.env.to_json()).expect("JSON");
            out.push(b'\n');
            let _ = io::stdout().write_all(&out);
            eprintln!("{}", env.out_path);
            ExitCode::SUCCESS
        }
        Err(code) => ExitCode::from(code as u8),
    }
}

fn die(msg: &str) -> ! {
    eprintln!("shell_env: {msg}");
    process::exit(2)
}
