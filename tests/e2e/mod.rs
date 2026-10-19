//! A nix-daemon on a private store dir, for tests that build. Each `Store`
//! has its own daemon, so tests run in parallel without root.

#![allow(dead_code)]

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime};
use std::{env, fs, thread};

use tempfile::TempDir;

use crate::hermetic;

const CONF: &str = "\
experimental-features = nix-command flakes
substituters =
trusted-users = *
build-users-group =
sandbox = false
";

pub struct Store {
    dir: TempDir,
    daemon: Child,
}

fn which(prog: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|d| d.join(prog))
        .find(|p| p.is_file())
}

/// `prog` on `PATH` in the real store, out of any profile.
fn resolved(prog: &str) -> PathBuf {
    let p = which(prog).unwrap_or_else(|| panic!("{prog} on PATH"));
    fs::canonicalize(p).unwrap()
}

impl Store {
    /// `None` without `nix-daemon` on `PATH`.
    pub fn start() -> Option<Store> {
        Store::start_with(&[])
    }

    pub fn start_with(opts: &[(&str, &str)]) -> Option<Store> {
        let daemon = which("nix-daemon")?;
        // a socket path has to fit in sun_path, and darwin's TMPDIR is long
        let dir = tempfile::Builder::new()
            .prefix("iets-e2e.")
            .tempdir_in("/tmp")
            .expect("tempdir");
        let d = dir.path();
        for sub in ["store", "var", "log", "conf"] {
            fs::create_dir(d.join(sub)).unwrap();
        }
        let mut conf = CONF.to_string();
        for (k, v) in opts {
            conf.push_str(&format!("{k} = {v}\n"));
        }
        fs::write(d.join("conf/nix.conf"), conf).unwrap();
        let mut cmd = Command::new(daemon);
        Store::env(d, &mut cmd);
        let log = fs::File::create(d.join("daemon.log")).unwrap();
        cmd.args(["--store", "local"])
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            // the per-connection children outlive the parent, so `Drop`
            // kills the whole group
            .process_group(0);
        let daemon = cmd.spawn().expect("spawn nix-daemon");
        let store = Store { dir, daemon };
        store.write_pkgs();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !store.socket().exists() {
            assert!(
                Instant::now() < deadline,
                "no daemon socket: {}",
                store.daemon_log()
            );
            thread::sleep(Duration::from_millis(20));
        }
        Some(store)
    }

    fn env(d: &Path, c: &mut Command) {
        c.env("NIX_STORE_DIR", d.join("store"))
            .env("NIX_STATE_DIR", d.join("var"))
            .env("NIX_LOG_DIR", d.join("log"))
            .env("NIX_DAEMON_SOCKET_PATH", d.join("socket"))
            .env("NIX_CONF_DIR", d.join("conf"))
            .env("NIX_USER_CONF_FILES", "")
            .env("XDG_CONFIG_HOME", d.join("xdg"))
            .env("XDG_CONFIG_DIRS", d.join("xdg"))
            .env("XDG_CACHE_HOME", d.join("cache"))
            .env("HOME", d)
            .env_remove("NIX_CONFIG")
            .env_remove("NIX_REMOTE")
            .env_remove("NIX_PATH");
    }

    /// `tests/e2e/pkgs`, with an `env.nix` that names this machine's bash and
    /// coreutils and a nonce, so every `Store` builds fresh paths.
    fn write_pkgs(&self) {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/pkgs");
        let dst = self.pkgs();
        fs::create_dir(&dst).unwrap();
        for e in fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            fs::copy(e.path(), dst.join(e.file_name())).unwrap();
        }
        let bash = resolved("bash");
        let coreutils = resolved("ls");
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let env = format!(
            "{{ shell = \"{}\"; path = \"{}\"; system = \"{}\"; nonce = \"{nonce}\"; }}\n",
            bash.display(),
            coreutils.parent().unwrap().display(),
            system(),
        );
        fs::write(dst.join("env.nix"), env).unwrap();
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    pub fn store_dir(&self) -> PathBuf {
        self.dir().join("store")
    }

    pub fn socket(&self) -> PathBuf {
        self.dir().join("socket")
    }

    /// The copy of `tests/e2e/pkgs` this store builds from.
    pub fn pkgs(&self) -> PathBuf {
        self.dir().join("pkgs")
    }

    pub fn daemon_pid(&self) -> u32 {
        self.daemon.id()
    }

    pub fn daemon_log(&self) -> String {
        fs::read_to_string(self.dir().join("daemon.log")).unwrap_or_default()
    }

    /// iets, talking to this daemon only.
    pub fn iets(&self) -> Command {
        let mut c = hermetic::iets();
        Store::env(self.dir(), &mut c);
        c.current_dir(self.dir());
        c
    }

    /// A Nix tool, talking to this daemon only.
    pub fn nix(&self, prog: &str) -> Command {
        let mut c = Command::new(prog);
        Store::env(self.dir(), &mut c);
        // with a writable state dir Nix would open the store itself
        c.env("NIX_REMOTE", "daemon").current_dir(self.dir());
        c
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // SAFETY: kill only sends a signal
        unsafe { libc::kill(-(self.daemon.id() as i32), libc::SIGKILL) };
        let _ = self.daemon.wait();
    }
}

/// Nix's `system` for this host.
pub fn system() -> String {
    let os = match env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{}-{os}", env::consts::ARCH)
}

/// Runs `c`, and panics with its stderr unless it succeeds.
pub fn ok(c: &mut Command) -> Output {
    let o = c.output().expect("spawn");
    assert!(
        o.status.success(),
        "{c:?} failed with {}:\n{}",
        o.status,
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim_end().to_string()
}

/// A Store, or an early return from a test that has no `nix-daemon`.
#[macro_export]
macro_rules! store {
    ($($opt:expr),* $(,)?) => {
        match e2e::Store::start_with(&[$($opt),*]) {
            Some(s) => s,
            None => {
                eprintln!("skipped: no nix-daemon on PATH");
                return;
            }
        }
    };
}
