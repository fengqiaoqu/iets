//! The resident evaluator, through the public API only.

use iets::api::host::HostBuiltin;
use iets::api::resident::Resident;
use iets::api::{EvalArgs, Input};
use iets::eval::EvalConfig;
use iets::session::Session;

fn double() -> HostBuiltin {
    HostBuiltin::new("double", 1, |e, args| {
        let v = e.force(args[0]).map_err(|err| err.msg.clone())?;
        let n = v.as_int().ok_or("expected an integer")?;
        Ok(e.int(n * 2))
    })
}

#[test]
fn a_resident_evaluator_serves_many_attributes() {
    let session = Session::new();
    let root = EvalArgs {
        input: Some(Input::Expr(
            "{ a = builtins.double 2; b.c = [ true ]; }".into(),
        )),
        no_tracing_cache: true,
        ..EvalArgs::new()
    };
    let cfg = EvalConfig::new()
        .session(session.clone())
        .host_builtin(double());
    let r = Resident::start(session, cfg, root);
    assert_eq!(r.eval_json("a").unwrap(), b"4");
    assert_eq!(r.eval_json("b").unwrap(), br#"{"c":[true]}"#);
    assert!(r.eval_json("nope").is_err());
    // an error does not stop it
    assert_eq!(r.eval_json("b.c").unwrap(), b"[true]");
}

#[test]
fn a_resident_entry_misses_when_an_earlier_read_changes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.nix");
    std::fs::write(&file, "1").unwrap();
    let src = format!("{{ x = import {}; y = 2; }}", file.display());
    let start = || {
        let session = Session::new();
        let root = EvalArgs {
            input: Some(Input::Expr(src.clone())),
            cache_dir: Some(dir.path().join("cache").display().to_string()),
            ..EvalArgs::new()
        };
        let cfg = EvalConfig::new().session(session.clone());
        Resident::start(session, cfg, root)
    };
    let r = start();
    assert_eq!(r.eval_json("x").unwrap(), b"1");
    // y never reads x.nix, but its entry holds every read before it
    assert_eq!(r.eval_json("y").unwrap(), b"2");
    drop(r);
    std::fs::write(&file, "10").unwrap();
    let r = start();
    assert_eq!(r.eval_json("y").unwrap(), b"2");
    assert_eq!(r.eval_json("x").unwrap(), b"10");
}

#[test]
fn a_resident_entry_hits_until_an_earlier_read_changes() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.nix");
    std::fs::write(&file, "1").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let src = format!("{{ x = import {}; y = builtins.count 2; }}", file.display());
    let start = || {
        let session = Session::new();
        let root = EvalArgs {
            input: Some(Input::Expr(src.clone())),
            cache_dir: Some(dir.path().join("cache").display().to_string()),
            ..EvalArgs::new()
        };
        let n = calls.clone();
        let count = HostBuiltin::new("count", 1, move |e, args| {
            n.fetch_add(1, Ordering::SeqCst);
            e.force(args[0]).map_err(|err| err.msg.clone())
        });
        let cfg = EvalConfig::new()
            .session(session.clone())
            .host_builtin(count);
        Resident::start(session, cfg, root)
    };
    let ask = |attr: &str| start().eval_json(attr).unwrap();
    let r = start();
    assert_eq!(r.eval_json("x").unwrap(), b"1");
    assert_eq!(r.eval_json("y").unwrap(), b"2");
    drop(r);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(ask("y"), b"2");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "unchanged inputs: a hit");
    std::fs::write(&file, "10").unwrap();
    assert_eq!(ask("y"), b"2");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "x.nix changed: a miss");
}
