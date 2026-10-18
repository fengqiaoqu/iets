//! One evaluator that outlives a call. A host such as a module system
//! evaluates one root expression and then reads many attributes of it; a
//! fresh run per attribute would evaluate the root again each time.
//!
//! Each answer is still a tracing-cache entry of its own, keyed on the root
//! and the attribute, and checked before the evaluator is touched. An entry
//! holds every observation the evaluator made so far: a superset of what
//! the answer read, which can only make it miss more often.

use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::api::installable::{drv_roots, find_along_attr_path};
use crate::api::run::Run;
use crate::api::shell_env::ShellEnv;
use crate::api::{self, EvalArgs};
use crate::cli::devenv::{DevEnvArgs, GcRoot, dev_env_of_roots, remembered_env};
use crate::eval::{EvalConfig, Evaluator, Pos};
use crate::print::print_json;
use crate::remote::worker::{Options, Transport};
use crate::runtime::Word;
use crate::session::Session;
use crate::trace::run::Cache;
use crate::trace::store::Part;

type Job = Box<dyn FnOnce(&mut Evaluator, &mut Root) + Send>;

struct Root {
    args: EvalArgs,
    cfg: EvalConfig,
    value: Option<Word>,
}

impl Root {
    /// The attribute `attr` of the root, evaluating the root on first use.
    fn select(&mut self, ev: &mut Evaluator, attr: &str) -> Result<Word, String> {
        let r = (|| {
            let root = match self.value {
                Some(v) => v,
                None => {
                    let v = api::evaluate(ev, self.root_args())?;
                    self.value = Some(v);
                    v
                }
            };
            let args = api::auto_args(ev, &[], &api::cwd())?;
            find_along_attr_path(ev, root, attr, args)
        })();
        r.map_err(|e| {
            api::report(ev, &e);
            e.msg.clone()
        })
    }

    fn root_args(&self) -> EvalArgs {
        EvalArgs {
            input: self.args.input.clone(),
            includes: self.args.includes.clone(),
            pure: self.args.pure,
            no_tracing_cache: self.args.no_tracing_cache,
            cache_dir: self.args.cache_dir.clone(),
            ..EvalArgs::new()
        }
    }

    /// The cache entry of `attr` under `subcommand`.
    fn cache(&self, attr: &str, subcommand: &str) -> Option<Cache> {
        let a = EvalArgs {
            attr: Some(attr.to_owned()),
            ..self.root_args()
        };
        api::setup_cache(&a, subcommand, &self.cfg)
    }
}

/// An evaluator on a thread of its own, with the evaluator's stack. Requests
/// run one at a time, in the order they arrive.
pub struct Resident {
    jobs: Option<mpsc::Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl Resident {
    /// `root` names the expression, with `input` and the search path;
    /// `cfg` is the configuration `api::Run::over` would make for it. The
    /// thread enters `session`.
    pub fn start(session: Arc<Session>, cfg: EvalConfig, root: EvalArgs) -> Resident {
        let (tx, rx) = mpsc::channel::<Job>();
        let thread = thread::Builder::new()
            .name("iets-resident".into())
            .stack_size(api::MAIN_STACK)
            .spawn(move || {
                let _entered = session.enter();
                let run = Run::on(cfg.clone()).cached(&root, "resident");
                let mut state = Root {
                    args: root,
                    cfg,
                    value: None,
                };
                let _ = run.go(move |ev, _| {
                    while let Ok(job) = rx.recv() {
                        job(ev, &mut state);
                    }
                });
            })
            .expect("spawn the resident evaluator");
        Resident {
            jobs: Some(tx),
            thread: Some(thread),
        }
    }

    fn ask<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Evaluator, &mut Root) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let (tx, rx) = mpsc::channel();
        let job: Job = Box::new(move |ev, root| {
            let _ = tx.send(f(ev, root));
        });
        self.jobs
            .as_ref()
            .ok_or("the resident evaluator is gone")?
            .send(job)
            .map_err(|_| "the resident evaluator is gone")?;
        rx.recv()
            .map_err(|_| "the resident evaluator stopped".to_string())?
    }

    /// The attribute `attr` of the root as strict JSON, as `eval --json`
    /// prints it.
    pub fn eval_json(&self, attr: &str) -> Result<Vec<u8>, String> {
        let attr = attr.to_owned();
        self.ask(move |ev, root| {
            let cache = root.cache(&attr, "resident-eval");
            if let Some(c) = &cache
                && c.lookup()
                && let Some(bytes) = c.part(Part::Result)
            {
                return Ok(bytes);
            }
            let v = root.select(ev, &attr)?;
            let mut out = Vec::new();
            print_json(ev, v, &mut out, &mut Vec::new(), true, Pos::NONE).map_err(|e| {
                api::report(ev, &e);
                e.msg.clone()
            })?;
            if let Some(c) = cache {
                c.write_kept(ev, &out);
            }
            Ok(out)
        })
    }

    /// The environment of the shell at `attr`, as `api::shell_env::shell_env`
    /// gives it, with its `-env` built through the daemon at `transport`.
    pub fn shell_env(
        &self,
        attr: &str,
        transport: Transport,
        options: Options,
        gc_root: GcRoot,
    ) -> Result<ShellEnv, String> {
        let attr = attr.to_owned();
        self.ask(move |ev, root| {
            let d = DevEnvArgs {
                transport,
                options,
                gc_root,
            };
            let local = matches!(d.transport, Transport::Unix(_));
            let cache = local
                .then(|| root.cache(&attr, "resident-develop"))
                .flatten();
            let entry = cache.as_ref().map(|c| c.entry_path());
            if let Some(c) = &cache
                && c.lookup()
                && let Some(env) = c.part(Part::Result).and_then(|r| remembered_env(&r))
            {
                d.gc_root
                    .register(&d.transport, &env.out_path, None)
                    .map_err(|e| e.to_string())?;
                return Ok(ShellEnv { entry, ..env });
            }
            let t0 = Instant::now();
            let v = root.select(ev, &attr)?;
            let mut roots = Vec::new();
            drv_roots(ev, v, &mut roots).map_err(|e| {
                api::report(ev, &e);
                e.msg.clone()
            })?;
            let env = dev_env_of_roots(ev, &attr, &roots, &d, false, t0)
                .map_err(|code| format!("the shell failed with exit code {code}"))?;
            if let Some(c) = cache {
                c.write_kept(ev, format!("{}\n{}", env.out_path, env.builder).as_bytes());
            }
            Ok(ShellEnv { entry, ..env })
        })
    }
}

impl Drop for Resident {
    fn drop(&mut self) {
        self.jobs.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
