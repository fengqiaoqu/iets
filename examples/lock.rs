//! Lock inputs declared in a JSON file, as a host would declare them.
//!
//! `cargo run --example lock -- INPUTS.json [OLD.lock] [--update all|a,b] [--unlocked-paths] [--base DIR]`
//!
//! INPUTS.json: `{ "name": { "url": …, "follows": …, "flake": …, "inputs": {…} } }`.
//! Prints the lock; says on stderr whether it changed and whether it fetched.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use iets::api::lock::{Input, Options, Update, lock};
use iets::eval::EvalConfig;

fn input(v: &serde_json::Value) -> Input {
    Input {
        url: v["url"].as_str().map(str::to_owned),
        follows: v["follows"].as_str().map(str::to_owned),
        flake: v["flake"].as_bool().unwrap_or(true),
        inputs: inputs(&v["inputs"]),
    }
}

fn inputs(v: &serde_json::Value) -> BTreeMap<String, Input> {
    v.as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), input(v)))
        .collect()
}

fn main() -> ExitCode {
    let session = iets::session::Session::new();
    session.log.write_to_stderr();
    let _entered = session.enter();
    let mut args = std::env::args().skip(1);
    let mut positional = Vec::new();
    let mut opts = Options {
        update: Update::Nothing,
        unlocked_paths: false,
        base: std::env::current_dir().unwrap(),
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--update" => {
                let v = args.next().unwrap();
                opts.update = match v.as_str() {
                    "all" => Update::All,
                    v => Update::Only(v.split(',').map(str::to_owned).collect()),
                }
            }
            "--unlocked-paths" => opts.unlocked_paths = true,
            "--base" => opts.base = PathBuf::from(args.next().unwrap()),
            _ => positional.push(a),
        }
    }
    let decl: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&positional[0]).unwrap()).unwrap();
    let old = positional.get(1).and_then(|p| std::fs::read(p).ok());
    let cfg = EvalConfig::new().pure(false).session(session.clone());
    match lock(cfg, &inputs(&decl), old.as_deref(), &opts) {
        Ok(l) => {
            let _ = std::io::stdout().write_all(&l.bytes);
            eprintln!("changed={} fetched={}", l.changed, l.fetched);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lock: {e}");
            ExitCode::FAILURE
        }
    }
}
