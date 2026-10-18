//! Locking inputs a host declares itself, as `nix flake lock` locks those
//! of a `flake.nix`. An entry of the old lock that still matches what is
//! declared is kept without a fetch, so locking a project whose lock is up
//! to date costs a read of the lock and no network.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::eval::EvalConfig;
use crate::flake::locker::{self, Decl, Locker, Want};
use crate::flake::original_of;

/// One input, as `flake.nix` declares it.
#[derive(Clone, Debug)]
pub struct Input {
    pub url: Option<String>,
    /// `a/b`: input `b` of the root's input `a`.
    pub follows: Option<String>,
    /// `false` for a tree that is not a flake: its inputs are not read.
    pub flake: bool,
    /// Overrides of the input's own inputs.
    pub inputs: BTreeMap<String, Input>,
}

impl Input {
    fn decl(&self) -> Result<Decl, String> {
        let want = match (&self.follows, &self.url) {
            (Some(f), _) => Want::Follows(
                f.split('/')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            ),
            (None, Some(u)) => {
                Want::Ref(original_of(u).ok_or_else(|| format!("'{u}' is not a flake reference"))?)
            }
            (None, None) => Want::None,
        };
        let inputs = self
            .inputs
            .iter()
            .map(|(k, i)| Ok((k.clone(), i.decl()?)))
            .collect::<Result<_, String>>()?;
        Ok(Decl {
            want,
            flake: self.flake,
            inputs,
        })
    }
}

/// Which inputs to lock again even when the old lock has them.
#[derive(Clone, Debug)]
pub enum Update {
    Nothing,
    All,
    /// Input paths, `a` or `a/b`; an input's own inputs come along.
    Only(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct Options {
    pub update: Update,
    /// Lock an absolute `path:` input without a `narHash`, as a tree read
    /// where it is: the lock then never goes stale when the tree changes.
    pub unlocked_paths: bool,
    /// The directory a relative `path:` input is relative to.
    pub base: PathBuf,
}

pub struct Locked {
    /// The lock as Nix writes it.
    pub bytes: Vec<u8>,
    /// Whether it differs from the old lock, so the host writes it.
    pub changed: bool,
    /// Whether an input had to be fetched to lock it.
    pub fetched: bool,
}

/// The lock of `inputs`, starting from `old`. Runs on a thread of its own
/// with the evaluator's stack, in the session the caller is in; an
/// evaluator is made only when an input has to be fetched.
pub fn lock(
    cfg: EvalConfig,
    inputs: &BTreeMap<String, Input>,
    old: Option<&[u8]>,
    opts: &Options,
) -> Result<Locked, String> {
    let decls = inputs
        .iter()
        .map(|(k, i)| Ok((k.clone(), i.decl()?)))
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    let lopts = locker::Options {
        update: match &opts.update {
            Update::Nothing => locker::Update::Nothing,
            Update::All => locker::Update::All,
            Update::Only(ps) => locker::Update::Only(
                ps.iter()
                    .map(|p| p.split('/').map(str::to_owned).collect())
                    .collect(),
            ),
        },
        unlocked_paths: opts.unlocked_paths,
        base: opts.base.clone(),
    };
    let old = old.map(<[u8]>::to_vec);
    let run = move || {
        let locker = Locker::new(cfg, &lopts, old.as_deref())?;
        let (text, fetched) = locker.lock(decls)?;
        let changed = old.as_deref() != Some(text.as_bytes());
        Ok(Locked {
            bytes: text.into_bytes(),
            changed,
            fetched,
        })
    };
    std::thread::Builder::new()
        .name("iets-lock".into())
        .stack_size(crate::api::MAIN_STACK)
        .spawn(crate::session::bind(run))
        .map_err(|e| format!("cannot start the locker: {e}"))?
        .join()
        .map_err(|_| "the locker panicked".to_string())?
}
