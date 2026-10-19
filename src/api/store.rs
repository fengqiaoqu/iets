//! A Nix store through its daemon, for a host that needs GC roots, path
//! queries, builds and collection without linking Nix. One connection,
//! opened on first use and kept; calls take turns on it.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub use crate::remote::event::Event;
pub use crate::remote::worker::Options;
use crate::remote::worker::{Connection, Transport};

/// What the daemon reports while it works: build logs, phases, failures.
pub type OnEvent = Arc<dyn Fn(&Event) + Send + Sync>;

pub struct PathInfo {
    pub nar_size: u64,
    pub references: Vec<String>,
}

/// gc-store.hh:GCOptions::GCAction.
#[derive(Clone, Copy, Debug)]
pub enum GcAction {
    ReturnLive = 0,
    ReturnDead = 1,
    DeleteDead = 2,
    DeleteSpecific = 3,
}

pub struct Store {
    transport: Transport,
    options: Mutex<Options>,
    on_event: OnEvent,
    conn: Mutex<Option<Connection>>,
}

impl Store {
    /// The store at `url`: `daemon`, `unix:///path` or `ssh-ng://…`.
    pub fn open(url: &str, options: Options, on_event: OnEvent) -> Result<Store, String> {
        let transport = Transport::parse(url)
            .ok_or_else(|| format!("'{url}' is not a store iets can reach"))?;
        Ok(Store {
            transport,
            options: Mutex::new(options),
            on_event,
            conn: Mutex::new(None),
        })
    }

    pub fn uri(&self) -> String {
        self.transport.uri()
    }

    fn with<T>(
        &self,
        f: impl FnOnce(&mut Connection, &mut dyn FnMut(Event)) -> crate::error::Result<T>,
    ) -> Result<T, String> {
        let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let on_event = self.on_event.clone();
        let mut events = move |e: Event| on_event(&e);
        if guard.is_none() {
            let options = self
                .options
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            let c = Connection::open_with(&self.transport, &options, &mut events)
                .map_err(|e| e.to_string())?;
            *guard = Some(c);
        }
        let conn = guard.as_mut().expect("opened above");
        let r = f(conn, &mut events);
        if r.is_err() {
            // a failed call may leave the stream mid-message
            *guard = None;
        }
        r.map_err(|e| e.to_string())
    }

    /// Options for the calls that follow, such as substituters as
    /// `overrides`: the daemon takes them at the handshake, so the
    /// connection is opened again.
    pub fn set_options(&self, options: Options) {
        *self.options.lock().unwrap_or_else(|p| p.into_inner()) = options;
        *self.conn.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Whether the daemon trusts this user; `None` when it does not say.
    pub fn trusted(&self) -> Result<Option<bool>, String> {
        self.with(|c, _| Ok(c.trusted))
    }

    /// Points `root` at `path` and registers it as an indirect root.
    pub fn add_perm_root(&self, path: &str, root: &Path) -> Result<(), String> {
        self.with(|c, _| c.add_perm_root(path, root))
    }

    pub fn path_info(&self, path: &str) -> Result<Option<PathInfo>, String> {
        self.with(|c, ev| c.query_path_info(path, ev)).map(|i| {
            i.map(|i| PathInfo {
                nar_size: i.nar_size,
                references: i.references,
            })
        })
    }

    /// The subset of `paths` the store has.
    pub fn valid(&self, paths: &[String]) -> Result<Vec<String>, String> {
        self.with(|c, ev| c.query_valid_paths(paths, ev))
    }

    /// Builds or substitutes every output of `drv`, which the store must
    /// have, and gives them by output name.
    pub fn realise(&self, drv: &str) -> Result<BTreeMap<String, String>, String> {
        let results = self.with(|c, ev| c.build_paths_with_results(&[format!("{drv}!*")], ev))?;
        if let Some(r) = results.iter().find(|r| !r.success()) {
            return Err(format!("cannot build '{drv}': {}", r.msg));
        }
        let outputs = self.with(|c, ev| c.query_derivation_output_map(drv, ev))?;
        outputs
            .into_iter()
            .map(|(name, p)| {
                Ok((
                    name.clone(),
                    p.ok_or_else(|| format!("'{drv}' has no path for output '{name}'"))?,
                ))
            })
            .collect()
    }

    /// Runs the collector; gives the paths it named and the bytes freed.
    pub fn collect_garbage(
        &self,
        action: GcAction,
        paths: &[String],
        ignore_liveness: bool,
        max_freed: u64,
    ) -> Result<(Vec<String>, u64), String> {
        self.with(|c, ev| c.collect_garbage(action as u64, paths, ignore_liveness, max_freed, ev))
    }
}
