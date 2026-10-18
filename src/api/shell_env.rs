//! The shell environment of a derivation, as `nix print-dev-env` computes it,
//! for a host that evaluates its own expression rather than a flake.

pub use crate::cli::devenv::{DevEnv as ShellEnv, GcRoot, dev_env as shell_env};
pub use crate::devenv::{BuildEnvironment, make_rc_script};
pub use crate::remote::worker::{Options, Transport};
