//! Host builtins, through the public API only.

use iets::api::host::HostBuiltin;
use iets::api::value::Eval;
use iets::eval::EvalConfig;

fn double() -> HostBuiltin {
    HostBuiltin::new("double", 1, |e, args| {
        let v = e.force(args[0]).map_err(|err| err.msg.clone())?;
        let n = v.as_int().ok_or("expected an integer")?;
        Ok(e.int(n * 2))
    })
}

fn eval_with(cfg: EvalConfig, src: &str) -> Result<i64, String> {
    let mut ev = cfg.build();
    let mut e = Eval::new(&mut ev);
    let v = e.eval_expr(src, b"/").map_err(|err| err.msg.clone())?;
    let v = e.force(v).map_err(|err| err.msg.clone())?;
    v.as_int().ok_or_else(|| "not an int".into())
}

fn on_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(iets::api::MAIN_STACK)
        .spawn(iets::session::bind(f))
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn a_host_builtin_answers_under_builtins() {
    on_stack(|| {
        let cfg = || EvalConfig::new().host_builtin(double());
        assert_eq!(eval_with(cfg(), "builtins.double 21"), Ok(42));
        // the argument stays lazy until the builtin forces it
        assert_eq!(eval_with(cfg(), "builtins.double (1 + 2)"), Ok(6));
        let err = eval_with(cfg(), r#"builtins.double "x""#).unwrap_err();
        assert!(
            err.contains("builtins.double: expected an integer"),
            "{err}"
        );
        // not a global
        assert!(eval_with(cfg(), "double 1").is_err());
    });
}

#[test]
fn a_host_builtin_replaces_a_builtin_of_its_name() {
    on_stack(|| {
        let cfg = EvalConfig::new().host_builtin(HostBuiltin::new("add", 1, |e, _| Ok(e.int(7))));
        assert_eq!(eval_with(cfg, "builtins.add 1"), Ok(7));
    });
}
