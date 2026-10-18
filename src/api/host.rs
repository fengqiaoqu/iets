//! Builtins a host adds: a Rust closure under a name of its choosing, called
//! through the value API (`api::value`). docs/reference/library.md, "Host
//! builtins", has the rules.

use std::collections::HashSet;
use std::fmt::{self, Debug, Formatter};
use std::sync::{Arc, Mutex};

use crate::api::value::{Eval, Value};
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::runtime::Word;

/// The function of a host builtin. It gets exactly `arity` arguments, none
/// of them forced. An `Err` becomes an evaluation error at the call site.
pub type HostFn =
    dyn for<'e> Fn(&mut Eval<'e>, &[Value<'e>]) -> Result<Value<'e>, String> + Send + Sync;

/// One host builtin; `EvalConfig::host_builtin` adds it to `builtins`.
#[derive(Clone)]
pub struct HostBuiltin {
    pub(crate) name: &'static str,
    pub(crate) arity: u8,
    pub(crate) f: Arc<HostFn>,
    pub(crate) impure: bool,
    pub(crate) key: Vec<u8>,
}

impl Debug for HostBuiltin {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "HostBuiltin({}/{})", self.name, self.arity)
    }
}

/// Primop names live as long as the process; a host that makes many
/// evaluators with the same builtins leaks each name once.
fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut names = NAMES.lock().unwrap();
    let names = names.get_or_insert_with(HashSet::new);
    if let Some(n) = names.get(name) {
        return n;
    }
    let n: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(n);
    n
}

impl HostBuiltin {
    /// `builtins.<name>`, a function of `arity` arguments (at least one).
    /// The result may depend only on the arguments and on files read through
    /// `Eval::read_file`; anything else needs [`HostBuiltin::impure`] or
    /// [`HostBuiltin::key`].
    pub fn new(
        name: &str,
        arity: u8,
        f: impl for<'e> Fn(&mut Eval<'e>, &[Value<'e>]) -> Result<Value<'e>, String>
        + Send
        + Sync
        + 'static,
    ) -> HostBuiltin {
        assert!(arity >= 1, "a host builtin takes at least one argument");
        HostBuiltin {
            name: intern(name),
            arity,
            f: Arc::new(f),
            impure: false,
            key: Vec::new(),
        }
    }

    /// The result depends on more than the arguments and the files read
    /// through `Eval::read_file`: a run that calls it writes no cache entry.
    pub fn impure(mut self) -> HostBuiltin {
        self.impure = true;
        self
    }

    /// Bytes that go into every cache key of a run with this builtin: a
    /// version, or the host state the result depends on.
    pub fn key(mut self, key: &[u8]) -> HostBuiltin {
        self.key = key.to_vec();
        self
    }
}

/// What the cache key holds for these builtins.
pub(crate) fn cache_key(host: &[HostBuiltin]) -> Vec<u8> {
    let mut k = Vec::new();
    for h in host {
        k.extend_from_slice(h.name.as_bytes());
        k.push(b'/');
        k.push(b'0' + h.arity.min(9));
        k.push(if h.impure { b'!' } else { b'.' });
        k.extend_from_slice(&h.key);
        k.push(0);
    }
    k
}

/// The primop every host builtin goes through: `args[0]` is the index into
/// the evaluator's host builtins, bound when `builtins` is made.
pub(crate) fn call(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let idx = ev
        .force(args[0])?
        .as_int()
        .expect("a host builtin is bound to its index") as usize;
    let h = ev.host[idx].clone();
    if h.impure {
        ev.trace().mark_uncacheable();
    }
    let mut e = Eval::new(ev);
    let vals: Vec<Value<'_>> = args[1..].iter().map(|w| Value::of(*w)).collect();
    match (h.f)(&mut e, &vals) {
        Ok(v) => ev_force(e, v),
        Err(msg) => Err(Error::boxed(
            ErrorKind::Eval,
            format!("builtins.{}: {msg}", h.name),
            pos,
        )),
    }
}

fn ev_force<'e>(mut e: Eval<'e>, v: Value<'e>) -> EvalResult<Word> {
    e.force(v).map(Value::word)
}
