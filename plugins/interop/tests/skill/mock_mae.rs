//! Mock maestro (`mae*`, `axl*`) functions for `sfy_ade.il` (spec `interop.md` §5.4, §9).
//!
//! Signatures (rows of `mockdb::SIGNATURES`, checked before a call reaches here) are the
//! IC23.1 *ADE SKILL Reference* syntax lines, as collected (MIT) by virtuoso-bridge-lite's
//! function database; the behaviour follows their descriptions and the notes in research
//! `virtuoso-netlist-ade.md` §5.3. What the reference leaves open is marked `GUESS(Gn)`
//! (spec §13).
//!
//! Model: a setup per maestro view (`saved`, written by `maeSaveSetup`); `maeOpenSetup`
//! gives a session (`fnxSession<k>`) holding a working copy, dropped by `maeCloseSession`.
//! A read-only session (`?mode "r"`) accepts edits in memory but never saves (as observed
//! on IC23.1). `axl*` handles are integers naming a session's setup database, a corner, a
//! model or a corner variable. Lists of tests, corners, variables and outputs come back in
//! reverse creation order (a dumper that forgets to sort fails); a corner's models and a
//! test's analysis options keep their order (they are ordered in the setup database).

use std::collections::BTreeMap;

use super::super::V;
use super::MockDb;

/// First `axl*` handle number (handles look like the reference's `7918`).
const HANDLE_BASE: i64 = 7000;

/// Sessions one run opens at most (a bound on the session table).
const SESSIONS_MAX: usize = 1 << 12;

/// One maestro setup: tests, global variables, corners.
#[derive(Clone, Debug, Default)]
pub struct Setup {
    /// Tests in creation order.
    pub tests: Vec<Test>,
    /// Global variables `(name, value)` in creation order.
    pub vars: Vec<(String, String)>,
    /// Corners in creation order.
    pub corners: Vec<Corner>,
}

/// An analysis `(name, enabled, options)`, options in the order set.
pub type MockAnalysis = (String, bool, Vec<(String, V)>);

/// One test.
#[derive(Clone, Debug, Default)]
pub struct Test {
    /// Test name.
    pub name: String,
    /// Design `(lib, cell, view)` and simulator.
    pub design: (String, String, String),
    /// Simulator name.
    pub simulator: String,
    /// Analyses `(name, enabled, options)`, options in the order set.
    pub analyses: Vec<MockAnalysis>,
    /// Outputs in creation order.
    pub outputs: Vec<Output>,
    /// Environment options `(name, value)`.
    pub env: Vec<(String, V)>,
    /// Test-level variables `(name, value)` in creation order: they override a global of
    /// the same name in this test.
    pub vars: Vec<(String, String)>,
    /// Simulator options `(name, value)` (`temp`, `reltol`, …).
    pub sim: Vec<(String, V)>,
}

/// One output of a test.
#[derive(Clone, Debug, Default)]
pub struct Output {
    /// Output name.
    pub name: String,
    /// `?outputType`.
    pub kind: String,
    /// `?signalName`, `?expr`.
    pub signal: Option<String>,
    /// See `signal`.
    pub expr: Option<String>,
    /// The spec: `(op, value)` pairs from the last `maeSetSpec`.
    pub spec: Vec<(String, V)>,
}

/// One corner.
#[derive(Clone, Debug, Default)]
pub struct Corner {
    /// Corner name.
    pub name: String,
    /// `?enabled`.
    pub enabled: bool,
    /// Corner variables `(name, value)` in creation order.
    pub vars: Vec<(String, String)>,
    /// Models `(name, file, section)` in creation order.
    pub models: Vec<(String, String, String)>,
}

/// One open session.
#[derive(Clone, Debug)]
pub struct Session {
    /// The maestro view's library and cell.
    pub lib: String,
    /// See `lib`.
    pub cell: String,
    /// `"r"` or `"a"`.
    pub mode: String,
    /// The working copy.
    pub setup: Setup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Handle {
    Sdb(usize),
    Corner(usize, String),
    Model(usize, String, String),
    Var(usize, String, String),
    Test(usize, String),
    TestVar(usize, String, String),
}

/// Maestro state: saved setups by (lib, cell), sessions, `axl*` handles, and the log of
/// every maestro call (`name` per call, for tests to check what the loader did).
#[derive(Default)]
pub struct Mae {
    /// Saved setups by (lib, cell).
    pub saved: BTreeMap<(String, String), Setup>,
    /// Sessions by number; `None` once closed.
    pub sessions: Vec<Option<Session>>,
    /// Names of the maestro functions called, in order.
    pub calls: Vec<String>,
    handles: Vec<Handle>,
}

type Kw = Vec<(String, V)>;

/// Positional arguments and `?key value` pairs (the signature check already ran).
fn split(args: &[V]) -> (Vec<V>, Kw) {
    let mut pos = Vec::new();
    let mut kw = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a {
            V::Sym(k) if k.starts_with('?') => {
                kw.push((k[1..].to_owned(), it.next().cloned().unwrap_or(V::Nil)));
            }
            _ => pos.push(a.clone()),
        }
    }
    (pos, kw)
}

fn key<'a>(kw: &'a Kw, k: &str) -> Option<&'a V> {
    kw.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

fn st(v: Option<&V>) -> Option<String> {
    match v {
        Some(V::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn strs(v: &V) -> Option<Vec<String>> {
    v.items().iter().map(|x| st(Some(x))).collect()
}

fn names<'a>(it: impl DoubleEndedIterator<Item = &'a String>) -> V {
    V::list(it.rev().map(|s| V::str(s)).collect())
}

fn tf(b: bool) -> V {
    if b { V::T } else { V::Nil }
}

impl Mae {
    /// The session `?session` names, else the latest open one (the "current" session).
    fn session(&self, kw: &Kw) -> Option<usize> {
        match st(key(kw, "session")) {
            Some(s) => {
                let k: usize = s.strip_prefix("fnxSession")?.parse().ok()?;
                self.sessions.get(k)?.as_ref().map(|_| k)
            }
            None => self.sessions.iter().rposition(Option::is_some),
        }
    }

    fn setup(&mut self, k: usize) -> &mut Setup {
        &mut self.sessions[k].as_mut().expect("open session").setup
    }

    fn handle(&mut self, h: Handle) -> V {
        let k = match self.handles.iter().position(|x| *x == h) {
            Some(k) => k,
            None => {
                self.handles.push(h);
                self.handles.len() - 1
            }
        };
        V::Int(HANDLE_BASE + k as i64)
    }

    fn resolve(&self, v: &V) -> Option<Handle> {
        let k = usize::try_from(v.as_int()? - HANDLE_BASE).ok()?;
        let h = self.handles.get(k)?.clone();
        let s = match &h {
            Handle::Sdb(s)
            | Handle::Corner(s, _)
            | Handle::Model(s, ..)
            | Handle::Var(s, ..)
            | Handle::Test(s, _)
            | Handle::TestVar(s, ..) => *s,
        };
        self.sessions.get(s)?.as_ref().map(|_| h)
    }

    fn corner(&mut self, s: usize, name: &str) -> Option<&mut Corner> {
        self.setup(s).corners.iter_mut().find(|c| c.name == name)
    }

    fn test(&mut self, s: usize, name: &str) -> Option<&mut Test> {
        self.setup(s).tests.iter_mut().find(|t| t.name == name)
    }
}

/// Handles a maestro function; `None` when `name` is not one.
pub fn call(db: &mut MockDb, name: &str, args: &[V]) -> Option<Result<V, String>> {
    if !(name.starts_with("mae") || name.starts_with("axl")) {
        return None;
    }
    db.mae.calls.push(name.to_owned());
    let (a, kw) = split(args);
    let s0 = |i: usize| st(a.get(i)).unwrap_or_default();
    let m = &mut db.mae;
    let r: Result<V, String> = match name {
        "maeOpenSetup" => {
            let (lib, cell, view) = (s0(0), s0(1), s0(2));
            let mode = st(key(&kw, "mode")).unwrap_or_else(|| "a".into());
            if !matches!(mode.as_str(), "r" | "a" | "w") {
                return Some(Err(format!("*Error* maeOpenSetup: mode {mode}")));
            }
            let locked = m
                .sessions
                .iter()
                .flatten()
                .any(|x| x.lib == lib && x.cell == cell && x.mode != "r");
            let full = m.sessions.len() >= SESSIONS_MAX;
            if db.lib(&lib).is_none() || (locked && mode != "r") || full {
                return Some(Ok(V::Nil));
            }
            // "If the given cellview does not exist, the function creates a new cellview."
            db.dd_get(&lib, Some(&cell), Some(&view), None, "a")
                .expect("library exists");
            let m = &mut db.mae;
            let setup = m
                .saved
                .get(&(lib.clone(), cell.clone()))
                .cloned()
                .unwrap_or_default();
            m.sessions.push(Some(Session {
                lib,
                cell,
                mode,
                setup,
            }));
            Ok(V::str(&format!("fnxSession{}", m.sessions.len() - 1)))
        }
        "maeCloseSession" => Ok(match m.session(&kw) {
            Some(k) => {
                m.sessions[k] = None;
                V::T
            }
            None => V::Nil,
        }),
        "maeSaveSetup" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let sess = m.sessions[k].as_ref().expect("open").clone();
            for (kk, want) in [("lib", &sess.lib), ("cell", &sess.cell)] {
                if st(key(&kw, kk)).is_some_and(|v| &v != want) {
                    return Some(Err(
                        "*Error* maeSaveSetup: saving to another view is not modelled".into(),
                    ));
                }
            }
            if sess.mode == "r" {
                Ok(V::Nil)
            } else {
                m.saved.insert((sess.lib, sess.cell), sess.setup);
                Ok(V::T)
            }
        }
        "maeGetSetup" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let su = m.setup(k);
            Ok(match st(key(&kw, "typeName")).as_deref() {
                None | Some("tests") => names(su.tests.iter().map(|t| &t.name)),
                // ADE lists its built-in nominal corner, whose handle is 0 (live run).
                Some("corners") => {
                    let nominal = "Nominal".to_owned();
                    names(std::iter::once(&nominal).chain(su.corners.iter().map(|c| &c.name)))
                }
                Some("variables") => names(su.vars.iter().map(|v| &v.0)),
                Some(_) => V::Nil,
            })
        }
        "maeCreateTest" => {
            if key(&kw, "sourceTest").is_some() {
                return Some(Err(
                    "*Error* maeCreateTest: ?sourceTest is not modelled".into()
                ));
            }
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let n = s0(0);
            if m.test(k, &n).is_some() {
                return Some(Ok(V::Nil));
            }
            let get = |kk: &str| st(key(&kw, kk)).unwrap_or_default();
            let t = Test {
                name: n,
                design: (get("lib"), get("cell"), get("view")),
                simulator: get("simulator"),
                ..Test::default()
            };
            m.setup(k).tests.push(t);
            Ok(V::T)
        }
        "maeDeleteTest" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let n = s0(0);
            let su = m.setup(k);
            let before = su.tests.len();
            su.tests.retain(|t| t.name != n);
            Ok(tf(su.tests.len() < before))
        }
        // GUESS(G20): option values are kept as given (strings from the loader).
        "maeSetAnalysis" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let mut opts = Vec::new();
            for o in key(&kw, "options").map_or(&[][..], V::items) {
                match o.items() {
                    [V::Str(kk), v] => opts.push((kk.to_string(), v.clone())),
                    _ => {
                        return Some(Err(
                            "*Error* maeSetAnalysis: ?options is a list of (name value)".into(),
                        ));
                    }
                }
            }
            let (tn, an) = (s0(0), s0(1));
            let enable = key(&kw, "enable").map(V::truthy);
            let Some(t) = m.test(k, &tn) else {
                return Some(Ok(V::Nil));
            };
            match t.analyses.iter_mut().find(|x| x.0 == an) {
                Some(x) => {
                    if let Some(e) = enable {
                        x.1 = e;
                    }
                    for (kk, v) in opts {
                        match x.2.iter_mut().find(|o| o.0 == kk) {
                            Some(o) => o.1 = v,
                            None => x.2.push((kk, v)),
                        }
                    }
                }
                None => t.analyses.push((an, enable.unwrap_or(true), opts)),
            }
            Ok(V::T)
        }
        "maeGetEnabledAnalysis" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            Ok(match m.test(k, &s0(0)) {
                Some(t) => names(t.analyses.iter().filter(|x| x.1).map(|x| &x.0)),
                None => V::Nil,
            })
        }
        "maeGetAnalysis" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let an = s0(1);
            // IC23.1 answers form fields it was never given: `anaName` first, and the ac
            // sweep chooser (virtuoso-bridge-lite maestro-skill-api.md: `(("anaName" "ac")
            // ("sweep" "Frequency") ("start" "1") …)`).
            Ok(m.test(k, &s0(0))
                .and_then(|t| t.analyses.iter().find(|x| x.0 == an))
                .map_or(V::Nil, |x| {
                    let mut l = vec![V::list(vec![V::str("anaName"), V::str(&an)])];
                    if an == "ac" && !x.2.iter().any(|o| o.0 == "sweep") {
                        l.push(V::list(vec![V::str("sweep"), V::str("Frequency")]));
                    }
                    l.extend(
                        x.2.iter()
                            .filter(|o| o.0 != "anaName")
                            .map(|(kk, v)| V::list(vec![V::str(kk), v.clone()])),
                    );
                    V::list(l)
                }))
        }
        "maeSetVar" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let (n, v) = (s0(0), s0(1));
            if st(a.get(1)).is_none() {
                return Some(Err("*Error* maeSetVar: the mock takes string values".into()));
            }
            let upsert = |vars: &mut Vec<(String, String)>| match vars.iter_mut().find(|x| x.0 == n)
            {
                Some(x) => x.1 = v.clone(),
                None => vars.push((n.clone(), v.clone())),
            };
            match st(key(&kw, "typeName")).as_deref() {
                None => {
                    upsert(&mut m.setup(k).vars);
                    Ok(V::T)
                }
                Some("corner") => {
                    let Some(cs) = key(&kw, "typeValue").and_then(strs) else {
                        return Some(Err(
                            "*Error* maeSetVar: ?typeValue is a list of names".into()
                        ));
                    };
                    if cs.is_empty()
                        || cs
                            .iter()
                            .any(|c| !m.setup(k).corners.iter().any(|x| &x.name == c))
                    {
                        return Some(Ok(V::Nil));
                    }
                    for c in cs {
                        upsert(&mut m.corner(k, &c).expect("checked").vars);
                    }
                    Ok(V::T)
                }
                Some("test") => {
                    let Some(ts) = key(&kw, "typeValue").and_then(strs) else {
                        return Some(Err(
                            "*Error* maeSetVar: ?typeValue is a list of names".into()
                        ));
                    };
                    if ts.is_empty() || ts.iter().any(|t| m.test(k, t).is_none()) {
                        return Some(Ok(V::Nil));
                    }
                    for t in ts {
                        upsert(&mut m.test(k, &t).expect("checked").vars);
                    }
                    Ok(V::T)
                }
                Some(t) => Err(format!("*Error* maeSetVar: ?typeName {t} is not modelled")),
            }
        }
        "maeGetVar" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let n = s0(0);
            let find =
                |vars: &[(String, String)]| vars.iter().find(|x| x.0 == n).map(|x| V::str(&x.1));
            Ok(match (
                st(key(&kw, "typeName")).as_deref(),
                st(key(&kw, "typeValue")),
            ) {
                (None, _) => find(&m.setup(k).vars),
                (Some("corner"), Some(c)) => m.corner(k, &c).and_then(|x| find(&x.vars)),
                _ => None,
            }
            .unwrap_or(V::Nil))
        }
        "maeSetCorner" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let n = s0(0);
            let en = key(&kw, "enabled").map(V::truthy);
            match m.corner(k, &n) {
                Some(c) => c.enabled = en.unwrap_or(c.enabled),
                None => m.setup(k).corners.push(Corner {
                    name: n,
                    enabled: en.unwrap_or(true),
                    ..Corner::default()
                }),
            }
            Ok(V::T)
        }
        "maeAddOutput" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let kind = st(key(&kw, "outputType")).unwrap_or_default();
            let (sig, ex) = (st(key(&kw, "signalName")), st(key(&kw, "expr")));
            let ok = match kind.as_str() {
                "point" => ex.is_some() && sig.is_none(),
                "net" => sig.is_some() && ex.is_none(),
                _ => false,
            };
            if !ok {
                return Some(Err(
                    "*Error* maeAddOutput: point takes ?expr, net ?signalName".into(),
                ));
            }
            let n = s0(0);
            let Some(t) = m.test(k, &s0(1)) else {
                return Some(Ok(V::Nil));
            };
            if t.outputs.iter().any(|o| o.name == n) {
                return Some(Ok(V::Nil));
            }
            t.outputs.push(Output {
                name: n,
                kind,
                signal: sig,
                expr: ex,
                spec: Vec::new(),
            });
            Ok(V::T)
        }
        "maeGetTestOutputs" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let outs = m
                .test(k, &s0(0))
                .map(|t| t.outputs.clone())
                .unwrap_or_default();
            let opt = |x: &Option<String>| x.as_deref().map_or(V::Nil, V::str);
            let mut objs = Vec::new();
            for o in outs.iter().rev() {
                let id = db.new_obj(
                    "sevOutputStruct",
                    vec![
                        ("name", V::str(&o.name)),
                        ("outputType", V::str(&o.kind)),
                        // The names IC23.1 answers (virtuoso-bridge-lite reads
                        // `o~>signal o~>expression` live); `~>expr`/`~>signalName` are nil.
                        ("signal", opt(&o.signal)),
                        ("expression", opt(&o.expr)),
                    ],
                );
                objs.push(V::Obj(id));
            }
            Ok(V::list(objs))
        }
        "maeSetSpec" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let mut spec = Vec::new();
            for op in ["gt", "lt", "range", "minimum", "maximum", "tolerance"] {
                if let Some(v) = key(&kw, op) {
                    let ok = match op {
                        // GUESS(G21): ?range takes (lo hi).
                        "range" => strs(v).is_some_and(|x| x.len() == 2),
                        _ => matches!(v, V::Str(_)),
                    };
                    if !ok {
                        return Some(Err(format!("*Error* maeSetSpec: bad ?{op}")));
                    }
                    spec.push((op.to_owned(), v.clone()));
                }
            }
            if spec.len() != 1 {
                return Some(Err(
                    "*Error* maeSetSpec: one of ?gt ?lt ?range ?minimum ?maximum ?tolerance".into(),
                ));
            }
            let o = s0(0);
            match m
                .test(k, &s0(1))
                .and_then(|t| t.outputs.iter_mut().find(|x| x.name == o))
            {
                Some(x) => {
                    x.spec = spec;
                    Ok(V::T)
                }
                None => Ok(V::Nil),
            }
        }
        "maeSetEnvOption" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let mut set = Vec::new();
            for o in key(&kw, "options").map_or(&[][..], V::items) {
                let [V::Str(n), v] = o.items() else {
                    return Some(Err(
                        "*Error* maeSetEnvOption: ?options is a list of (name value)".into(),
                    ));
                };
                let ok = match &**n {
                    "modelFiles" => v
                        .items()
                        .iter()
                        .all(|e| strs(e).is_some_and(|x| (1..=2).contains(&x.len()))),
                    // GUESS(G22): a list of paths.
                    "definitionFiles" => strs(v).is_some(),
                    _ => true,
                };
                if !ok {
                    return Some(Err(format!("*Error* maeSetEnvOption: bad {n}")));
                }
                set.push((n.to_string(), v.clone()));
            }
            let Some(t) = m.test(k, &s0(0)) else {
                return Some(Ok(V::Nil));
            };
            for (n, v) in set {
                t.env.retain(|x| x.0 != n);
                t.env.push((n, v));
            }
            Ok(V::T)
        }
        "maeGetEnvOption" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let opt = st(key(&kw, "option"));
            let Some(t) = m.test(k, &s0(0)) else {
                return Some(Ok(V::Nil));
            };
            Ok(match opt {
                Some(o) => t
                    .env
                    .iter()
                    .find(|x| x.0 == o)
                    .map_or(V::Nil, |x| x.1.clone()),
                None => V::list(
                    t.env
                        .iter()
                        .map(|(n, v)| V::list(vec![V::str(n), v.clone()]))
                        .collect(),
                ),
            })
        }
        // GUESS(G28): maeGetSimOption ?option names one option and answers its value as
        // maeGetEnvOption does; maeSetSimOption takes (name value) strings.
        "maeSetSimOption" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let mut set = Vec::new();
            for o in key(&kw, "options").map_or(&[][..], V::items) {
                let [V::Str(n), v @ V::Str(_)] = o.items() else {
                    return Some(Err(
                        "*Error* maeSetSimOption: ?options is a list of (name value) strings"
                            .into(),
                    ));
                };
                set.push((n.to_string(), v.clone()));
            }
            let Some(t) = m.test(k, &s0(0)) else {
                return Some(Ok(V::Nil));
            };
            for (n, v) in set {
                t.sim.retain(|x| x.0 != n);
                t.sim.push((n, v));
            }
            Ok(V::T)
        }
        "maeGetSimOption" => {
            let Some(k) = m.session(&kw) else {
                return Some(Ok(V::Nil));
            };
            let opt = st(key(&kw, "option"));
            let Some(t) = m.test(k, &s0(0)) else {
                return Some(Ok(V::Nil));
            };
            Ok(match opt {
                Some(o) => t
                    .sim
                    .iter()
                    .find(|x| x.0 == o)
                    .map_or(V::Nil, |x| x.1.clone()),
                None => V::list(
                    t.sim
                        .iter()
                        .map(|(n, v)| V::list(vec![V::str(n), v.clone()]))
                        .collect(),
                ),
            })
        }
        "axlGetTest" => match m.resolve(&a[0]) {
            Some(Handle::Sdb(k)) => {
                let n = s0(1);
                Ok(if m.test(k, &n).is_some() {
                    m.handle(Handle::Test(k, n))
                } else {
                    V::Nil
                })
            }
            _ => Err("*Error* axlGetTest: argument #1 should be a setup database handle".into()),
        },
        "axlGetMainSetupDB" => {
            let kw2 = vec![("session".to_owned(), V::str(&s0(0)))];
            Ok(match m.session(&kw2) {
                Some(k) => m.handle(Handle::Sdb(k)),
                None => V::Nil,
            })
        }
        "axlGetCorner" => match m.resolve(&a[0]) {
            Some(Handle::Sdb(k)) => {
                let n = s0(1);
                Ok(if n == "Nominal" {
                    V::Int(0)
                } else if m.corner(k, &n).is_some() {
                    m.handle(Handle::Corner(k, n))
                } else {
                    V::Nil
                })
            }
            _ => Err("*Error* axlGetCorner: argument #1 should be a setup database handle".into()),
        },
        "axlGetEnabled" => match m.resolve(&a[0]) {
            Some(Handle::Corner(k, c)) => Ok(tf(m.corner(k, &c).is_some_and(|x| x.enabled))),
            _ => Err("*Error* axlGetEnabled: the mock models corner handles only".into()),
        },
        "axlGetModels" => match m.resolve(&a[0]) {
            // GUESS(G19): the model names, in setup order.
            Some(Handle::Corner(k, c)) => Ok(m.corner(k, &c).map_or(V::Nil, |x| {
                V::list(x.models.iter().map(|md| V::str(&md.0)).collect())
            })),
            _ => Err("*Error* axlGetModels: argument #1 should be a corner handle".into()),
        },
        "axlGetModel" | "axlPutModel" => match m.resolve(&a[0]) {
            Some(Handle::Corner(k, c)) => {
                let n = s0(1);
                let x = m.corner(k, &c).expect("live handle");
                let has = x.models.iter().any(|md| md.0 == n);
                if !has && name == "axlGetModel" {
                    Ok(V::Int(0)) // "=> x_modelFile / 0"
                } else {
                    if !has {
                        x.models.push((n.clone(), String::new(), String::new()));
                    }
                    Ok(m.handle(Handle::Model(k, c, n)))
                }
            }
            _ => Err(format!(
                "*Error* {name}: argument #1 should be a corner handle"
            )),
        },
        "axlGetModelFile" | "axlGetModelSection" | "axlSetModelFile" | "axlSetModelSection" => {
            match m.resolve(&a[0]) {
                Some(Handle::Model(k, c, n)) => {
                    let h = a[0].clone();
                    let md = m
                        .corner(k, &c)
                        .and_then(|x| x.models.iter_mut().find(|md| md.0 == n))
                        .expect("live handle");
                    let field = if name.ends_with("File") {
                        &mut md.1
                    } else {
                        &mut md.2
                    };
                    if name.starts_with("axlSet") {
                        *field = s0(1);
                        Ok(if name.ends_with("File") {
                            V::str(field)
                        } else {
                            h
                        })
                    } else if field.is_empty() {
                        Ok(V::Nil)
                    } else {
                        Ok(V::str(field))
                    }
                }
                _ => Err(format!(
                    "*Error* {name}: argument #1 should be a model handle"
                )),
            }
        }
        "axlGetVars" => match m.resolve(&a[0]) {
            // "a list containing a handle to all variables … and a list of all variable names".
            Some(Handle::Corner(k, c)) => {
                let ns = m
                    .corner(k, &c)
                    .map_or(V::Nil, |x| names(x.vars.iter().map(|v| &v.0)));
                let h = m.handle(Handle::Corner(k, c));
                Ok(V::list(vec![h, ns]))
            }
            Some(Handle::Test(k, t)) => {
                let ns = m
                    .test(k, &t)
                    .map_or(V::Nil, |x| names(x.vars.iter().map(|v| &v.0)));
                let h = m.handle(Handle::Test(k, t));
                Ok(V::list(vec![h, ns]))
            }
            _ => Err("*Error* axlGetVars: the mock models corner and test handles only".into()),
        },
        "axlGetVar" => match m.resolve(&a[0]) {
            Some(Handle::Corner(k, c)) => {
                let n = s0(1);
                let has = m
                    .corner(k, &c)
                    .is_some_and(|x| x.vars.iter().any(|v| v.0 == n));
                Ok(if has {
                    m.handle(Handle::Var(k, c, n))
                } else {
                    V::Nil
                })
            }
            Some(Handle::Test(k, t)) => {
                let n = s0(1);
                let has = m
                    .test(k, &t)
                    .is_some_and(|x| x.vars.iter().any(|v| v.0 == n));
                Ok(if has {
                    m.handle(Handle::TestVar(k, t, n))
                } else {
                    V::Nil
                })
            }
            _ => Err("*Error* axlGetVar: the mock models corner and test handles only".into()),
        },
        "axlGetVarValue" => match m.resolve(&a[0]) {
            Some(Handle::Var(k, c, n)) => Ok(m
                .corner(k, &c)
                .and_then(|x| x.vars.iter().find(|v| v.0 == n))
                .map_or(V::Nil, |v| V::str(&v.1))),
            Some(Handle::TestVar(k, t, n)) => Ok(m
                .test(k, &t)
                .and_then(|x| x.vars.iter().find(|v| v.0 == n))
                .map_or(V::Nil, |v| V::str(&v.1))),
            _ => Err("*Error* axlGetVarValue: argument #1 should be a variable handle".into()),
        },
        "axlGetSpecData" => match m.resolve(&a[0]) {
            // One flat (op value...) list, the last spec set: ("gt" "1"), ("range" "1" "2")
            // (live ICADVM20.1 probe; G21 had a list of (op value) pairs).
            Some(Handle::Sdb(k)) => {
                let o = s0(1);
                Ok(m.test(k, &s0(2))
                    .and_then(|t| t.outputs.iter().find(|x| x.name == o))
                    .and_then(|x| x.spec.last())
                    .map_or(V::Nil, |(op, v)| {
                        let mut items = vec![V::str(op)];
                        match v {
                            V::List(l) => items.extend(l.iter().cloned()),
                            other => items.push(other.clone()),
                        }
                        V::list(items)
                    }))
            }
            _ => {
                Err("*Error* axlGetSpecData: argument #1 should be a setup database handle".into())
            }
        },
        _ => return None,
    };
    Some(r)
}
