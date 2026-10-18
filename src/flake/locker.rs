//! flake.cc:computeLocks: the lock of a set of declared inputs. An input
//! whose entry in the old lock still matches what is declared keeps it, with
//! its own inputs read from the old lock, so a lock that is up to date is
//! rebuilt without a fetch or an evaluation. Anything else is resolved,
//! fetched and hashed, and its `flake.nix` is read for its own inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::builtins::fetch::path_nar_hash;
use crate::eval::{EvalConfig, Evaluator, Pos};
use crate::flake::original_of;
use crate::runtime::{AttrsObj, Value as V};
use crate::source::lock::normal;
use crate::source::scm;
use crate::source::{AttrVal, Attrs, remote};
use crate::trace::Trace;

type Json = Map<String, Value>;
type Path_ = Vec<String>;

#[derive(Clone, Debug)]
pub enum Want {
    /// An input path from the root.
    Follows(Path_),
    /// The `original` of the reference.
    Ref(Json),
    None,
}

#[derive(Clone, Debug)]
pub struct Decl {
    pub want: Want,
    pub flake: bool,
    pub inputs: BTreeMap<String, Decl>,
}

#[derive(Clone, Debug)]
pub enum Update {
    Nothing,
    All,
    Only(Vec<Path_>),
}

pub struct Options {
    pub update: Update,
    /// Lock an absolute `path:` input as it is named, without a `narHash`:
    /// the tree is read where it is, so it never goes stale.
    pub unlocked_paths: bool,
    /// The directory a relative `path:` input of the root is relative to.
    pub base: PathBuf,
}

enum Edge {
    Node(usize),
    Follows(Path_),
}

struct Node {
    original: Json,
    locked: Json,
    flake: bool,
    parent: Option<Value>,
    inputs: BTreeMap<String, Edge>,
}

struct OldLock {
    nodes: Json,
    root: String,
}

impl OldLock {
    fn parse(bytes: &[u8]) -> Result<OldLock, String> {
        let v: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("cannot parse a lock: {e}"))?;
        Ok(OldLock {
            nodes: v["nodes"].as_object().cloned().unwrap_or_default(),
            root: v["root"].as_str().unwrap_or("root").to_string(),
        })
    }

    fn node(&self, key: &str) -> Option<&Json> {
        self.nodes.get(key)?.as_object()
    }
}

/// A node of an old lock: the project's own (`lock` 0), or the `flake.lock`
/// of an input that was fetched, whose `follows` are relative to `prefix`.
#[derive(Clone)]
struct OldRef {
    lock: usize,
    key: String,
    prefix: Path_,
}

pub struct Locker<'a> {
    opts: &'a Options,
    locks: Vec<OldLock>,
    nodes: Vec<Node>,
    overrides: BTreeMap<Path_, Decl>,
    cfg: EvalConfig,
    ev: Option<Evaluator>,
    registry: Option<Vec<(Json, Json)>>,
    /// Whether anything was fetched: a lock built from the old one alone.
    pub fetched: bool,
}

impl<'a> Locker<'a> {
    pub fn new(
        cfg: EvalConfig,
        opts: &'a Options,
        old: Option<&[u8]>,
    ) -> Result<Locker<'a>, String> {
        let locks = match old {
            None => Vec::new(),
            Some(bytes) => vec![OldLock::parse(bytes)?],
        };
        Ok(Locker {
            opts,
            locks,
            nodes: Vec::new(),
            overrides: BTreeMap::new(),
            cfg,
            ev: None,
            registry: None,
            fetched: false,
        })
    }

    fn ev(&mut self) -> &mut Evaluator {
        let cfg = &self.cfg;
        self.ev.get_or_insert_with(|| cfg.clone().build())
    }

    /// The lock of `inputs`, the root's declared inputs, as Nix writes it.
    pub fn lock(mut self, inputs: BTreeMap<String, Decl>) -> Result<(String, bool), String> {
        self.nodes.push(Node {
            original: Json::new(),
            locked: Json::new(),
            flake: true,
            parent: None,
            inputs: BTreeMap::new(),
        });
        let old_root = self.locks.first().map(|o| OldRef {
            lock: 0,
            key: o.root.clone(),
            prefix: Vec::new(),
        });
        self.compute(0, inputs, Vec::new(), old_root, None)?;
        let fetched = self.fetched;
        Ok((render(&self.nodes), fetched))
    }

    /// An update concerns the project's own lock: the inputs of an input
    /// fetched again still come from that input's `flake.lock`.
    fn must_update(&self, path: &[String], old: &OldRef) -> bool {
        old.lock == 0
            && match &self.opts.update {
                Update::Nothing => false,
                Update::All => true,
                Update::Only(ps) => ps.iter().any(|p| p == path),
            }
    }

    fn add_overrides(&mut self, at: &Path_, nested: &BTreeMap<String, Decl>) {
        for (k, d) in nested {
            let mut p = at.clone();
            p.push(k.clone());
            self.overrides.entry(p.clone()).or_insert_with(|| d.clone());
            self.add_overrides(&p, &d.inputs);
        }
    }

    /// `node`'s inputs from `decls`. `old` is the node's entry in the old
    /// lock, `dir` where its tree is, for relative paths.
    fn compute(
        &mut self,
        node: usize,
        decls: BTreeMap<String, Decl>,
        prefix: Path_,
        old: Option<OldRef>,
        dir: Option<PathBuf>,
    ) -> Result<(), String> {
        for (name, d) in &decls {
            let mut p = prefix.clone();
            p.push(name.clone());
            self.add_overrides(&p, &d.inputs);
        }
        for (name, decl) in decls {
            let mut path = prefix.clone();
            path.push(name.clone());
            let (want, flake) = match self.overrides.get(&path) {
                Some(o) if !matches!(o.want, Want::None) => (o.want.clone(), o.flake),
                _ => (decl.want, decl.flake),
            };
            let original = match want {
                Want::Follows(p) => {
                    self.nodes[node].inputs.insert(name, Edge::Follows(p));
                    continue;
                }
                Want::Ref(o) => o,
                Want::None => {
                    let mut o = Json::new();
                    o.insert("id".into(), name.clone().into());
                    o.insert("type".into(), "indirect".into());
                    o
                }
            };
            let old_child = old.as_ref().and_then(|r| {
                let o = &self.locks[r.lock];
                let key = o
                    .node(&r.key)?
                    .get("inputs")?
                    .get(&name)?
                    .as_str()?
                    .to_string();
                let child = o.node(&key)?;
                let same = child.get("original").and_then(Value::as_object).map(normal)
                    == Some(normal(&original));
                let same_flake =
                    child.get("flake").and_then(Value::as_bool).unwrap_or(true) == flake;
                (same && same_flake && child.get("locked").is_some()).then(|| OldRef {
                    lock: r.lock,
                    key,
                    prefix: r.prefix.clone(),
                })
            });
            if let Some(r) = old_child.filter(|r| !self.must_update(&path, r)) {
                let o = &self.locks[r.lock];
                let child = o.node(&r.key).expect("checked above");
                let mut sub = BTreeMap::new();
                for (k, spec) in child
                    .get("inputs")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                {
                    let d = match spec {
                        Value::Array(p) => Decl {
                            want: Want::Follows(
                                r.prefix
                                    .iter()
                                    .cloned()
                                    .chain(p.iter().filter_map(|s| s.as_str().map(str::to_owned)))
                                    .collect(),
                            ),
                            flake: true,
                            inputs: BTreeMap::new(),
                        },
                        Value::String(n) => match o.node(n) {
                            Some(c) => Decl {
                                want: Want::Ref(
                                    c.get("original")
                                        .and_then(Value::as_object)
                                        .cloned()
                                        .unwrap_or_default(),
                                ),
                                flake: c.get("flake").and_then(Value::as_bool).unwrap_or(true),
                                inputs: BTreeMap::new(),
                            },
                            None => continue,
                        },
                        _ => continue,
                    };
                    sub.insert(k.clone(), d);
                }
                let idx = self.nodes.len();
                self.nodes.push(Node {
                    original: child
                        .get("original")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default(),
                    locked: child
                        .get("locked")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default(),
                    flake,
                    parent: child.get("parent").cloned(),
                    inputs: BTreeMap::new(),
                });
                self.nodes[node].inputs.insert(name, Edge::Node(idx));
                self.compute(idx, sub, path, Some(r), None)?;
                continue;
            }
            let (locked, tree, parent) = self.fetch(&original, &path, dir.as_deref())?;
            self.fetched = true;
            let mut deps = None;
            let sub = if flake {
                let subdir = locked.get("dir").and_then(Value::as_str).unwrap_or("");
                let flake_dir = tree.join(subdir);
                if let Some(bytes) = read_tree_file(&flake_dir.join("flake.lock")) {
                    let l = OldLock::parse(&bytes)?;
                    deps = Some(OldRef {
                        lock: self.locks.len(),
                        key: l.root.clone(),
                        prefix: path.clone(),
                    });
                    self.locks.push(l);
                }
                self.flake_inputs(&flake_dir.join("flake.nix"), &path)?
            } else {
                BTreeMap::new()
            };
            let idx = self.nodes.len();
            self.nodes.push(Node {
                original,
                locked,
                flake,
                parent,
                inputs: BTreeMap::new(),
            });
            self.nodes[node].inputs.insert(name, Edge::Node(idx));
            self.compute(idx, sub, path, deps, Some(tree))?;
        }
        Ok(())
    }

    fn registry(&mut self) -> &[(Json, Json)] {
        if self.registry.is_none() {
            let r = remote::flake_registry(self.ev());
            self.registry = Some(r);
        }
        self.registry.as_deref().unwrap_or(&[])
    }

    /// registry.cc:lookupInRegistries: the reference an `indirect` one
    /// stands for, with its `ref` and `rev` carried over.
    fn resolve(&mut self, original: &Json) -> Result<Json, String> {
        let id = original.get("id").and_then(Value::as_str).unwrap_or("");
        let found = self.registry().iter().find(|(from, _)| {
            from.get("id").and_then(Value::as_str) == Some(id)
                && ["ref", "rev"]
                    .iter()
                    .all(|k| from.get(*k).is_none_or(|v| original.get(*k) == Some(v)))
        });
        let Some((from, to)) = found else {
            return Err(format!(
                "cannot find flake 'flake:{id}' in the flake registries"
            ));
        };
        let mut to = to.clone();
        for k in ["ref", "rev"] {
            if from.get(k).is_none()
                && let Some(v) = original.get(k)
            {
                to.insert(k.into(), v.clone());
            }
        }
        Ok(to)
    }

    /// The `locked` of `original`, the directory its tree is readable at,
    /// and the `parent` of a relative path.
    fn fetch(
        &mut self,
        original: &Json,
        path: &[String],
        parent_dir: Option<&Path>,
    ) -> Result<(Json, PathBuf, Option<Value>), String> {
        let shown = path.join("/");
        let ty = original.get("type").and_then(Value::as_str).unwrap_or("");
        let resolved = if ty == "indirect" {
            self.resolve(original)?
        } else {
            original.clone()
        };
        let dir = resolved.get("dir").cloned();
        let mut attrs: Attrs = resolved
            .iter()
            .filter(|(k, _)| *k != "dir")
            .filter_map(|(k, v)| Some((k.clone(), json_attr(v)?)))
            .collect();
        let get = |a: &Attrs, k: &str| match a.get(k) {
            Some(AttrVal::Str(s)) => Some(s.clone()),
            _ => None,
        };
        let pos = Pos::NONE;
        let err = |e: Box<crate::eval::Error>| format!("cannot lock input '{shown}': {}", e.msg);
        let ty = get(&attrs, "type").unwrap_or_default();
        let mut parent = None;
        let (mut locked, tree): (Json, PathBuf) = match ty.as_str() {
            "github" | "gitlab" | "sourcehut" => {
                if get(&attrs, "rev").is_none() {
                    let ev = self.ev();
                    let (url, headers) = remote::forge_git_url(ev, &attrs)
                        .map_err(|e| format!("cannot lock input '{shown}': {e}"))?;
                    let refs = remote::ls_remote(ev, &url, &headers)
                        .map_err(|e| format!("cannot lock input '{shown}': {e}"))?;
                    let want = get(&attrs, "ref");
                    let (rev, _) = remote::pick_ref(&refs, want.as_deref()).ok_or_else(|| {
                        format!(
                            "cannot lock input '{shown}': {url} has no ref '{}'",
                            want.as_deref().unwrap_or("HEAD")
                        )
                    })?;
                    attrs.insert("rev".into(), AttrVal::Str(rev));
                }
                attrs.remove("ref");
                let (sp, hash, newest, _) =
                    remote::fetch_unlocked_tarball(self.ev(), &attrs, pos).map_err(err)?;
                let mut l: Json = attrs
                    .iter()
                    .map(|(k, v)| (k.clone(), attr_value(v)))
                    .collect();
                l.insert("narHash".into(), hash.to_sri().into());
                l.insert("lastModified".into(), newest.into());
                (l, PathBuf::from(sp.to_full_string()))
            }
            "tarball" | "file" => {
                attrs.insert("type".into(), AttrVal::Str("tarball".into()));
                let (sp, hash, newest, immutable) =
                    remote::fetch_unlocked_tarball(self.ev(), &attrs, pos).map_err(err)?;
                let mut l: Json = attrs
                    .iter()
                    .map(|(k, v)| (k.clone(), attr_value(v)))
                    .collect();
                if let Some(u) = immutable {
                    l.extend(immutable_attrs(&u));
                }
                l.insert("narHash".into(), hash.to_sri().into());
                l.entry("lastModified").or_insert(newest.into());
                (l, PathBuf::from(sp.to_full_string()))
            }
            "git" => {
                let url = get(&attrs, "url")
                    .ok_or_else(|| format!("cannot lock input '{shown}': no 'url'"))?;
                let want_rev = get(&attrs, "rev");
                let want_ref = get(&attrs, "ref");
                let git = scm::named("git").ok_or("iets knows no git")?;
                let (root, rev) = match url.strip_prefix("file://") {
                    Some(local) => {
                        let root = PathBuf::from(local);
                        let rev = match &want_rev {
                            Some(r) => r.clone(),
                            None => git
                                .resolve(
                                    &Trace::default(),
                                    &root,
                                    want_ref.as_deref().unwrap_or("HEAD"),
                                )?
                                .ok_or_else(|| {
                                    format!("cannot lock input '{shown}': no such ref")
                                })?,
                        };
                        (root, rev)
                    }
                    None => remote::git_clone(&url, want_rev.as_deref().or(want_ref.as_deref()))
                        .map_err(|e| format!("cannot lock input '{shown}': {e}"))?,
                };
                let commit = git
                    .tree_at(&Trace::default(), &root, &rev, false)?
                    .ok_or_else(|| {
                        format!("cannot lock input '{shown}': git has no revision {}", rev)
                    })?;
                let last_modified = commit.last_modified;
                let rev_count = git.rev_count(&root, &rev)?;
                let (sp, hash) =
                    remote::store_commit(self.ev(), "source", commit, pos).map_err(err)?;
                let mut l = Json::new();
                l.insert("lastModified".into(), last_modified.into());
                l.insert("narHash".into(), hash.to_sri().into());
                if let Some(r) = &want_ref {
                    let full = if r.starts_with("refs/") {
                        r.clone()
                    } else {
                        format!("refs/heads/{r}")
                    };
                    l.insert("ref".into(), full.into());
                }
                l.insert("rev".into(), rev.into());
                l.insert("revCount".into(), rev_count.into());
                l.insert("type".into(), "git".into());
                l.insert("url".into(), url.into());
                (l, PathBuf::from(sp.to_full_string()))
            }
            "path" => {
                let p = get(&attrs, "path")
                    .ok_or_else(|| format!("cannot lock input '{shown}': no 'path'"))?;
                let relative = !p.starts_with('/');
                let at = if relative {
                    match parent_dir {
                        Some(d) => d.join(&p),
                        None => self.opts.base.join(&p),
                    }
                } else {
                    PathBuf::from(&p)
                };
                if relative {
                    parent = Some(Value::Array(
                        path[..path.len() - 1]
                            .iter()
                            .map(|s| s.clone().into())
                            .collect(),
                    ));
                    let l = attrs
                        .iter()
                        .map(|(k, v)| (k.clone(), attr_value(v)))
                        .collect();
                    (l, at)
                } else if self.opts.unlocked_paths {
                    let l = attrs
                        .iter()
                        .map(|(k, v)| (k.clone(), attr_value(v)))
                        .collect();
                    (l, at)
                } else {
                    let (hash, mtime) = path_nar_hash(self.ev(), &p, pos).map_err(err)?;
                    let mut l: Json = attrs
                        .iter()
                        .map(|(k, v)| (k.clone(), attr_value(v)))
                        .collect();
                    l.insert("narHash".into(), hash.to_sri().into());
                    l.insert("lastModified".into(), mtime.into());
                    (l, at)
                }
            }
            other => {
                return Err(format!(
                    "cannot lock input '{shown}': iets cannot lock a '{other}' input"
                ));
            }
        };
        if let Some(d) = dir {
            locked.insert("dir".into(), d);
        }
        Ok((locked, tree, parent))
    }

    /// The inputs `flake.nix` declares, its `outputs` formals included.
    fn flake_inputs(
        &mut self,
        flake_nix: &Path,
        at: &Path_,
    ) -> Result<BTreeMap<String, Decl>, String> {
        let at = at.clone();
        let p = flake_nix.as_os_str().as_encoded_bytes().to_vec();
        let ev = self.ev();
        let r = (|| {
            let pos = Pos::NONE;
            let v = ev.eval_file(&p, pos)?;
            let attrs = ev.force_attrs(v, pos, "while evaluating flake.nix")?;
            let mut out = BTreeMap::new();
            if let Some(a) = attrs.get(ev.sym(b"inputs")) {
                let a = ev.force_attrs(
                    a.cell.get(),
                    pos,
                    "while evaluating the 'inputs' of flake.nix",
                )?;
                out = decls_of(ev, a, &at)?;
            }
            if let Some(o) = attrs.get(ev.sym(b"outputs")) {
                let o = ev.force(o.cell.get())?;
                if let V::Lambda(l) = o.view()
                    && let crate::compile::ir::ParamIr::Formals { formals, .. } = &l.lam.param
                {
                    for f in formals.iter() {
                        let name = String::from_utf8_lossy(ev.interner.name(f.sym)).into_owned();
                        if name != "self" && !out.contains_key(&name) {
                            out.insert(
                                name,
                                Decl {
                                    want: Want::None,
                                    flake: true,
                                    inputs: BTreeMap::new(),
                                },
                            );
                        }
                    }
                }
            }
            Ok::<_, Box<crate::eval::Error>>(out)
        })();
        r.map_err(|e| {
            format!(
                "cannot read the inputs of '{}': {}",
                flake_nix.display(),
                e.msg
            )
        })
    }
}

fn decls_of(
    ev: &mut Evaluator,
    a: &'static AttrsObj,
    base: &Path_,
) -> crate::eval::EvalResult<BTreeMap<String, Decl>> {
    let pos = Pos::NONE;
    let mut out = BTreeMap::new();
    for i in a.iter() {
        let name = String::from_utf8_lossy(ev.interner.name(i.sym)).into_owned();
        let d = ev.force_attrs(i.cell.get(), pos, "while evaluating a flake input")?;
        out.insert(name, decl_of(ev, d, base)?);
    }
    Ok(out)
}

fn decl_of(
    ev: &mut Evaluator,
    d: &'static AttrsObj,
    base: &Path_,
) -> crate::eval::EvalResult<Decl> {
    let pos = Pos::NONE;
    let mut want = Want::None;
    let mut flake = true;
    let mut inputs = BTreeMap::new();
    let mut fields = Json::new();
    for i in d.iter() {
        let k = String::from_utf8_lossy(ev.interner.name(i.sym)).into_owned();
        let v = i.cell.get();
        match k.as_str() {
            "url" => {
                let s = ev.force_string_no_ctx(v, pos, "while evaluating a flake input's url")?;
                let s = String::from_utf8_lossy(s).into_owned();
                if let Some(o) = original_of(&s) {
                    want = Want::Ref(o);
                }
            }
            "follows" => {
                let s = ev.force_string_no_ctx(v, pos, "while evaluating a follows attribute")?;
                let mut p = base.clone();
                p.extend(
                    String::from_utf8_lossy(s)
                        .split('/')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                );
                want = Want::Follows(p);
            }
            "flake" => {
                if let V::Bool(b) = ev.force(v)?.view() {
                    flake = b;
                }
            }
            "inputs" => {
                let a = ev.force_attrs(v, pos, "while evaluating the 'inputs' of a flake input")?;
                inputs = decls_of(ev, a, base)?;
            }
            _ => {
                let jv = match ev.force(v)?.view() {
                    V::Bool(b) => Value::Bool(b),
                    V::Int(n) => Value::from(n),
                    V::String(s) => Value::String(String::from_utf8_lossy(s.bytes).into_owned()),
                    _ => continue,
                };
                fields.insert(k, jv);
            }
        }
    }
    if matches!(want, Want::None) && fields.contains_key("type") {
        want = Want::Ref(fields);
    }
    Ok(Decl {
        want,
        flake,
        inputs,
    })
}

fn json_attr(v: &Value) -> Option<AttrVal> {
    match v {
        Value::String(s) => Some(AttrVal::Str(s.clone())),
        Value::Bool(b) => Some(AttrVal::Bool(*b)),
        Value::Number(n) => n.as_u64().map(AttrVal::Int),
        _ => None,
    }
}

fn attr_value(v: &AttrVal) -> Value {
    match v {
        AttrVal::Str(s) => Value::String(s.clone()),
        AttrVal::Bool(b) => Value::Bool(*b),
        AttrVal::Int(n) => Value::from(*n),
    }
}

/// lockfile.cc:LockFile::toJSON, printed as nlohmann's `dump(2)`: nodes are
/// named depth first in input order, a taken name gets `_2`, `_3`, ...
fn render(nodes: &[Node]) -> String {
    fn name(
        nodes: &[Node],
        i: usize,
        key: String,
        names: &mut BTreeMap<usize, String>,
        taken: &mut BTreeSet<String>,
        out: &mut Json,
    ) -> String {
        if let Some(k) = names.get(&i) {
            return k.clone();
        }
        let mut key = key;
        if !taken.insert(key.clone()) {
            for n in 2.. {
                let k = format!("{key}_{n}");
                if taken.insert(k.clone()) {
                    key = k;
                    break;
                }
            }
        }
        names.insert(i, key.clone());
        let node = &nodes[i];
        let mut n = Json::new();
        if !node.inputs.is_empty() {
            let mut inputs = Json::new();
            for (k, e) in &node.inputs {
                let v = match e {
                    Edge::Node(c) => Value::String(name(nodes, *c, k.clone(), names, taken, out)),
                    Edge::Follows(p) => Value::Array(p.iter().map(|s| s.clone().into()).collect()),
                };
                inputs.insert(k.clone(), v);
            }
            n.insert("inputs".into(), Value::Object(inputs));
        }
        if i != 0 {
            n.insert("locked".into(), Value::Object(node.locked.clone()));
            n.insert("original".into(), Value::Object(node.original.clone()));
            if !node.flake {
                n.insert("flake".into(), Value::Bool(false));
            }
            if let Some(p) = &node.parent {
                n.insert("parent".into(), p.clone());
            }
        }
        out.insert(key.clone(), Value::Object(n));
        key
    }
    let mut out = Json::new();
    let root = name(
        nodes,
        0,
        "root".into(),
        &mut BTreeMap::new(),
        &mut BTreeSet::new(),
        &mut out,
    );
    let mut top = Json::new();
    top.insert("nodes".into(), Value::Object(out));
    top.insert("root".into(), Value::String(root));
    top.insert("version".into(), Value::from(7));
    let mut s = String::new();
    dump(&Value::Object(top), 0, &mut s);
    s
}

/// nlohmann's `dump(2)`: keys sorted, two spaces a level, `": "`.
fn dump(v: &Value, level: usize, s: &mut String) {
    let pad = |s: &mut String, l: usize| s.extend(std::iter::repeat_n(' ', l * 2));
    match v {
        Value::Object(m) if m.is_empty() => s.push_str("{}"),
        Value::Object(m) => {
            s.push_str("{\n");
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            for (i, k) in keys.iter().enumerate() {
                pad(s, level + 1);
                s.push_str(&serde_json::to_string(k).expect("a string"));
                s.push_str(": ");
                dump(&m[*k], level + 1, s);
                if i + 1 < keys.len() {
                    s.push(',');
                }
                s.push('\n');
            }
            pad(s, level);
            s.push('}');
        }
        Value::Array(a) if a.is_empty() => s.push_str("[]"),
        Value::Array(a) => {
            s.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                pad(s, level + 1);
                dump(x, level + 1, s);
                if i + 1 < a.len() {
                    s.push(',');
                }
                s.push('\n');
            }
            pad(s, level);
            s.push(']');
        }
        other => s.push_str(&serde_json::to_string(other).expect("a scalar")),
    }
}

/// tarball.cc: the attributes an immutable URL locks a tarball input to.
/// `rev`, `revCount` and `lastModified` ride in its query and leave it.
fn immutable_attrs(u: &str) -> Json {
    let mut out = Json::new();
    let (base, query) = u.split_once('?').unwrap_or((u, ""));
    let mut kept = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "rev" => {
                out.insert("rev".into(), v.into());
            }
            "revCount" | "lastModified" => {
                if let Ok(n) = v.parse::<u64>() {
                    out.insert(k.into(), n.into());
                }
            }
            "narHash" => {}
            _ => kept.push(pair),
        }
    }
    let url = if kept.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", kept.join("&"))
    };
    out.insert("url".into(), url.into());
    out
}

/// A file of a fetched tree: on disk, or in the input store, which holds a
/// tree that `/nix/store` does not.
fn read_tree_file(p: &Path) -> Option<Vec<u8>> {
    if let Ok(b) = std::fs::read(p) {
        return Some(b);
    }
    let rel = p.strip_prefix(crate::store::path::store_dir()).ok()?;
    let mut parts = rel.components();
    let base = parts.next()?.as_os_str().to_str()?;
    let rest = parts.as_path().as_os_str().as_encoded_bytes();
    crate::treestore::get(base)?.read(rest).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Options {
        Options {
            update: Update::Nothing,
            unlocked_paths: true,
            base: PathBuf::from("/"),
        }
    }

    fn url(u: &str) -> Decl {
        Decl {
            want: Want::Ref(original_of(u).unwrap()),
            flake: true,
            inputs: BTreeMap::new(),
        }
    }

    const LOCK: &str = r#"{
  "nodes": {
    "a": {
      "inputs": {
        "nixpkgs": [
          "nixpkgs"
        ]
      },
      "locked": {
        "lastModified": 1,
        "narHash": "sha256-AAAA",
        "owner": "o",
        "repo": "a",
        "rev": "1111111111111111111111111111111111111111",
        "type": "github"
      },
      "original": {
        "owner": "o",
        "repo": "a",
        "type": "github"
      }
    },
    "b": {
      "flake": false,
      "locked": {
        "path": "/src/b",
        "type": "path"
      },
      "original": {
        "path": "/src/b",
        "type": "path"
      }
    },
    "nixpkgs": {
      "locked": {
        "lastModified": 2,
        "narHash": "sha256-BBBB",
        "owner": "NixOS",
        "repo": "nixpkgs",
        "rev": "2222222222222222222222222222222222222222",
        "type": "github"
      },
      "original": {
        "owner": "NixOS",
        "ref": "nixos-unstable",
        "repo": "nixpkgs",
        "type": "github"
      }
    },
    "root": {
      "inputs": {
        "a": "a",
        "b": "b",
        "nixpkgs": "nixpkgs"
      }
    }
  },
  "root": "root",
  "version": 7
}"#;

    fn decls() -> BTreeMap<String, Decl> {
        let mut a = url("github:o/a");
        a.inputs.insert(
            "nixpkgs".into(),
            Decl {
                want: Want::Follows(vec!["nixpkgs".into()]),
                flake: true,
                inputs: BTreeMap::new(),
            },
        );
        let mut b = url("path:/src/b");
        b.flake = false;
        BTreeMap::from([
            ("a".into(), a),
            ("b".into(), b),
            ("nixpkgs".into(), url("github:NixOS/nixpkgs/nixos-unstable")),
        ])
    }

    #[test]
    fn an_up_to_date_lock_comes_back_byte_for_byte_without_a_fetch() {
        let o = opts();
        let l = Locker::new(EvalConfig::new(), &o, Some(LOCK.as_bytes())).unwrap();
        let (text, fetched) = l.lock(decls()).unwrap();
        assert!(!fetched);
        assert_eq!(text, LOCK);
    }

    #[test]
    fn a_removed_input_leaves_the_lock() {
        let o = opts();
        let mut d = decls();
        d.remove("b");
        let l = Locker::new(EvalConfig::new(), &o, Some(LOCK.as_bytes())).unwrap();
        let (text, fetched) = l.lock(d).unwrap();
        assert!(!fetched);
        assert!(!text.contains("\"b\""), "{text}");
    }

    #[test]
    fn an_unlocked_path_is_locked_as_named() {
        let o = opts();
        let mut b = url("path:/src/c");
        b.flake = false;
        let l = Locker::new(EvalConfig::new(), &o, None).unwrap();
        let (text, fetched) = l.lock(BTreeMap::from([("c".into(), b)])).unwrap();
        assert!(fetched);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["nodes"]["c"]["locked"], v["nodes"]["c"]["original"]);
        assert_eq!(v["nodes"]["c"]["flake"], Value::Bool(false));
    }

    #[test]
    fn a_taken_node_name_gets_a_suffix() {
        let leaf = |i: usize| Node {
            original: Json::from_iter([("type".to_string(), Value::from("path"))]),
            locked: Json::from_iter([("n".to_string(), Value::from(i))]),
            flake: true,
            parent: None,
            inputs: BTreeMap::new(),
        };
        let mut root = leaf(0);
        let mut a = leaf(1);
        a.inputs.insert("x".into(), Edge::Node(3));
        root.inputs.insert("a".into(), Edge::Node(1));
        root.inputs.insert("x".into(), Edge::Node(2));
        let text = render(&[root, a, leaf(2), leaf(3)]);
        let v: Value = serde_json::from_str(&text).unwrap();
        // depth first: a's x is named before the root's
        assert_eq!(v["nodes"]["a"]["inputs"]["x"], "x");
        assert_eq!(v["nodes"]["root"]["inputs"]["x"], "x_2");
        assert_eq!(v["nodes"]["x"]["locked"]["n"], 3);
    }

    #[test]
    fn immutable_urls_carry_rev_and_rev_count() {
        let a = immutable_attrs("https://h/f/x.tar.gz?rev=abc&revCount=69&narHash=sha256-x&keep=1");
        assert_eq!(a["url"], "https://h/f/x.tar.gz?keep=1");
        assert_eq!(a["rev"], "abc");
        assert_eq!(a["revCount"], 69);
        assert!(a.get("narHash").is_none());
    }
}
