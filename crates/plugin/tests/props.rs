//! Properties of the host's pure transforms (TESTING.md `plugin` row), no wasm involved:
//! stateful fuzz of output records against a naive model (malformed streams, stale
//! documents, conflicting contributions, full queues), composition laws (last wins,
//! unload == compose without, idempotence), and capability decisions.

use std::collections::BTreeMap;

use proptest::prelude::*;
use schemify_plugin::abi::{Code, Kind, Out, Table, col, text};
use schemify_plugin::{
    Composed, Contributions, EDITS_MAX, Grants, IoOp, LOGS_MAX, Override, PluginId, RUNS_MAX,
    RequestQueues, Snapshots, TIMER_MS_MAX, TIMERS_MAX, authorize, compose, intake,
};

const PLUGINS: [PluginId; 3] = [PluginId(1), PluginId(2), PluginId(3)];
const KINDS: [Kind; 5] = [
    Kind::Ui,
    Kind::Commands,
    Kind::Keymap,
    Kind::Annotations,
    Kind::Functions,
];
const SCOPES: [&str; 2] = ["erc", "lvs pass"];
const LIVE_DOC: u32 = 1;
const NEAR_MISSES: [&str; 8] = [
    "frob 1",
    "timer 1",
    "log loud x",
    "contribute nonsense",
    "edit x commit l\n  | op",
    "contribute ui\n  attr x",
    "log info \"open",
    "contribute commands\n  | node x item",
];

/// A composed record as (kind, scope, key, owner, text).
type Row = (Kind, String, String, PluginId, String);
/// Members of one (kind, scope) in first-appearance order: (key, owner, text).
type Group = Vec<(String, PluginId, String)>;

/// One generated output record; the generator knows whether it is valid.
#[derive(Clone, Debug)]
enum Rec {
    /// A whole set: (name index, variant) per record. Always valid.
    Contribute {
        kind: Kind,
        scope: usize,
        recs: Vec<(u8, u8)>,
    },
    Edit {
        doc: u32,
    },
    Run,
    Timer {
        ms: u32,
    },
    Log(String),
    /// Invalid UTF-8 without a line break: exactly one rejected chunk.
    Garbage(Vec<u8>),
    NearMiss(usize),
}

/// One set member: its text and its composition key, derived independently of the host.
fn member(kind: Kind, name: u8, variant: u8) -> (String, String) {
    let n = ["a", "b", "c", "d"][name as usize % 4];
    match kind {
        Kind::Ui => match variant % 3 {
            0 => (
                format!("node {n} item\n  label \"{n}\""),
                format!("node {n}"),
            ),
            1 => (format!("remove {n}"), format!("node {n}")),
            _ => (
                format!("set {n} label x{variant}"),
                format!("set {n} label"),
            ),
        },
        Kind::Commands => (
            format!("command cmd.{n} \"L{variant}\""),
            format!("command cmd.{n}"),
        ),
        Kind::Keymap => (
            format!("key ctrl+{n} cmd.{n}{variant}"),
            format!("key ctrl+{n}"),
        ),
        Kind::Annotations => (format!("mark {n} warn \"m{variant}\""), format!("mark {n}")),
        Kind::Functions => (
            format!("function ns.{n} {variant} 2"),
            format!("function ns.{n}"),
        ),
        _ => unreachable!("not generated"),
    }
}

fn scope_of(kind: Kind, scope: usize) -> &'static str {
    if kind.scoped() { SCOPES[scope % 2] } else { "" }
}

fn render(r: &Rec, out: &mut Vec<u8>) {
    let mut o = Out::new();
    match r {
        Rec::Contribute { kind, scope, recs } => {
            let body: Vec<String> = recs.iter().map(|&(n, v)| member(*kind, n, v).0).collect();
            let body = body.join("\n");
            if kind.scoped() {
                o.contribute_scoped(*kind, scope_of(*kind, *scope), &body);
            } else {
                o.contribute(*kind, &body);
            }
        }
        Rec::Edit { doc } => o.edit(
            *doc,
            schemify_plugin::abi::EditMode::Stage,
            "size",
            "set_attr M1 w 2u",
        ),
        Rec::Run => o.run("lib:tb", "default", "tt", &[("vdd", "1.8")]),
        Rec::Timer { ms } => o.timer(7, *ms),
        Rec::Log(t) => o.log(t),
        Rec::Garbage(bytes) => {
            out.push(0xFF);
            out.extend(bytes.iter().filter(|&&b| b != b'\n'));
            out.push(b'\n');
        }
        Rec::NearMiss(i) => {
            out.extend_from_slice(NEAR_MISSES[*i].as_bytes());
            out.push(b'\n');
        }
    }
    out.extend_from_slice(o.as_str().as_bytes());
}

fn rec() -> impl Strategy<Value = Rec> {
    prop_oneof![
        4 => (0..KINDS.len(), 0..2usize, proptest::collection::vec((0u8..4, 0u8..6), 0..4))
            .prop_map(|(k, scope, recs)| Rec::Contribute { kind: KINDS[k], scope, recs }),
        1 => prop_oneof![Just(LIVE_DOC), Just(2u32), any::<u32>()].prop_map(|doc| Rec::Edit { doc }),
        1 => Just(Rec::Run),
        1 => prop_oneof![0..=TIMER_MS_MAX, Just(TIMER_MS_MAX + 1), any::<u32>()].prop_map(|ms| Rec::Timer { ms }),
        1 => "\\PC{0,12}".prop_map(Rec::Log),
        1 => proptest::collection::vec(any::<u8>(), 0..16).prop_map(Rec::Garbage),
        1 => (0..NEAR_MISSES.len()).prop_map(Rec::NearMiss),
    ]
}

#[derive(Clone, Debug)]
enum Step {
    Output { plugin: usize, recs: Vec<Rec> },
    Unload(usize),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        8 => (0..3usize, proptest::collection::vec(rec(), 0..10)).prop_map(|(plugin, recs)| Step::Output { plugin, recs }),
        1 => (0..3usize).prop_map(Step::Unload),
    ]
}

fn snapshots() -> Snapshots {
    let mut s = Snapshots::new();
    s.fill::<u32>(Table::Docs, col::docs::HANDLE, &[LIVE_DOC]);
    s.fill::<u8>(Table::Docs, col::docs::KIND, &[1]);
    s.fill::<u8>(Table::Docs, col::docs::DIRTY, &[0]);
    s.fill_strings(Table::Docs, col::docs::PATH, &["top.shcomp"]);
    s.fill_strings(Table::Docs, col::docs::CELL, &["top"]);
    s
}

/// The naive model: current sets as (key, text) lists, and queue lengths.
#[derive(Default)]
struct Model {
    sets: BTreeMap<(PluginId, Kind, String), Vec<(String, String)>>,
    edits: usize,
    runs: usize,
    timers: usize,
    logs: usize,
}

impl Model {
    /// Applies one record; returns the code the host must reject it with, if any.
    fn apply(&mut self, plugin: PluginId, r: &Rec) -> Option<Code> {
        let bounded = |n: &mut usize, max: usize| {
            if *n >= max {
                Some(Code::Space)
            } else {
                *n += 1;
                None
            }
        };
        match r {
            Rec::Contribute { kind, scope, recs } => {
                let key = (plugin, *kind, scope_of(*kind, *scope).to_owned());
                let members: Vec<(String, String)> = recs
                    .iter()
                    .map(|&(n, v)| member(*kind, n, v))
                    .map(|(t, k)| (k, t))
                    .collect();
                if members.is_empty() {
                    self.sets.remove(&key);
                } else {
                    self.sets.insert(key, members);
                }
                None
            }
            Rec::Edit { doc } if *doc != LIVE_DOC => Some(Code::Gone),
            Rec::Edit { .. } => bounded(&mut self.edits, EDITS_MAX),
            Rec::Run => bounded(&mut self.runs, RUNS_MAX),
            Rec::Timer { ms } if *ms > TIMER_MS_MAX => Some(Code::Range),
            Rec::Timer { .. } => bounded(&mut self.timers, TIMERS_MAX),
            Rec::Log(_) => bounded(&mut self.logs, LOGS_MAX),
            Rec::Garbage(_) | Rec::NearMiss(_) => Some(Code::Parse),
        }
    }

    /// Reference composition: for every key the last plugin in `order` wins; keys keep
    /// their first-appearance order per (kind, scope); a `node X` / `remove X` drops every
    /// `set X …` before it (a set after it appears again, at the end).
    fn compose(&self, order: &[PluginId]) -> Vec<Row> {
        let mut groups: BTreeMap<(Kind, String), Group> = BTreeMap::new();
        for &p in order {
            for ((owner, kind, scope), members) in &self.sets {
                if *owner != p {
                    continue;
                }
                let group = groups.entry((*kind, scope.clone())).or_default();
                for (key, text) in members {
                    if let Some(node) = key.strip_prefix("node ") {
                        let prefix = format!("set {node} ");
                        group.retain(|g| !g.0.starts_with(&prefix));
                    }
                    match group.iter_mut().find(|g| g.0 == *key) {
                        Some(g) => (g.1, g.2) = (p, text.clone()),
                        None => group.push((key.clone(), p, text.clone())),
                    }
                }
            }
        }
        groups
            .into_iter()
            .flat_map(|((kind, scope), g)| {
                g.into_iter()
                    .map(move |(key, p, text)| (kind, scope.clone(), key, p, text))
            })
            .collect()
    }
}

fn flatten(c: &Composed) -> Vec<Row> {
    c.records
        .iter()
        .map(|r| {
            (
                r.kind,
                r.scope.to_string(),
                r.key.to_string(),
                r.owner,
                r.text.to_string(),
            )
        })
        .collect()
}

fn feed(
    plugin: PluginId,
    recs: &[Rec],
    snap: &Snapshots,
    c: &mut Contributions,
    q: &mut RequestQueues,
) -> Vec<Code> {
    let mut bytes = Vec::new();
    for r in recs {
        render(r, &mut bytes);
    }
    let mut rejections = Vec::new();
    let accepted = intake(plugin, &bytes, snap, c, q, &mut rejections);
    assert_eq!(
        accepted as usize + rejections.len(),
        recs.len(),
        "one chunk per record"
    );
    assert!(
        rejections
            .iter()
            .all(|r| r.plugin == plugin && r.record.len() <= 256)
    );
    rejections.iter().map(|r| r.code).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn intake_and_compose_match_the_model(steps in proptest::collection::vec(step(), 1..16)) {
        let snap = snapshots();
        let (mut c, mut q, mut model) = (Contributions::new(), RequestQueues::default(), Model::default());
        let mut live = PLUGINS.to_vec();
        for s in &steps {
            match s {
                Step::Unload(i) => {
                    c.remove_plugin(PLUGINS[*i]);
                    model.sets.retain(|k, _| k.0 != PLUGINS[*i]);
                    live.retain(|&p| p != PLUGINS[*i]);
                }
                Step::Output { plugin, recs } => {
                    let p = PLUGINS[*plugin];
                    if !live.contains(&p) {
                        continue;
                    }
                    let got = feed(p, recs, &snap, &mut c, &mut q);
                    let want: Vec<Code> = recs.iter().filter_map(|r| model.apply(p, r)).collect();
                    prop_assert_eq!(got, want);
                }
            }
            prop_assert_eq!(c.len(), model.sets.len());
            for ((p, kind, scope), members) in &model.sets {
                let body: Vec<&str> = members.iter().map(|m| m.1.as_str()).collect();
                let body = body.join("\n");
                prop_assert_eq!(c.get(*p, *kind, scope), Some(body.as_str()));
            }
            prop_assert_eq!(
                (q.edits.len(), q.runs.len(), q.timers.len(), q.logs.len()),
                (model.edits, model.runs, model.timers, model.logs)
            );
            let (composed, overrides) = compose(&c, &live);
            let reference = model.compose(&live);
            prop_assert_eq!(flatten(&composed), reference.clone());
            let total: usize = model.sets.iter().filter(|(k, _)| live.contains(&k.0)).map(|(_, m)| m.len()).sum();
            prop_assert_eq!(overrides.len(), total - reference.len(), "one override per replaced record");
            let mut names: Vec<String> = reference
                .iter()
                .filter(|r| r.0 == Kind::Functions)
                .map(|r| r.2.trim_start_matches("function ").to_owned())
                .collect();
            names.sort();
            let registry: Vec<String> = composed.functions.name.iter().map(|n| n.to_string()).collect();
            prop_assert_eq!(registry, names);
        }
    }

    #[test]
    fn last_plugin_in_order_wins(
        sets in proptest::collection::vec(proptest::collection::vec((0u8..4, 0u8..6), 1..5), 3),
        order in Just(PLUGINS.to_vec()).prop_shuffle(),
    ) {
        let snap = snapshots();
        let (mut c, mut q, mut model) = (Contributions::new(), RequestQueues::default(), Model::default());
        for (p, recs) in PLUGINS.iter().zip(&sets) {
            let r = Rec::Contribute { kind: Kind::Ui, scope: 0, recs: recs.clone() };
            model.apply(*p, &r);
            prop_assert!(feed(*p, &[r], &snap, &mut c, &mut q).is_empty());
        }
        let (composed, overrides) = compose(&c, &order);
        prop_assert_eq!(flatten(&composed), model.compose(&order));
        for r in &composed.records {
            let has_key = |b: &str| b.lines().any(|l| !l.starts_with(' ') && line_key(l) == *r.key);
            let last = order.iter().rev().find(|&&p| c.get(p, Kind::Ui, "").is_some_and(has_key));
            prop_assert_eq!(Some(&r.owner), last);
        }
        let rank = |p: PluginId| order.iter().position(|&q| q == p);
        let ordered = overrides.iter().all(|o: &Override| rank(o.loser) <= rank(o.winner));
        prop_assert!(ordered, "an override never goes backwards in plugin order");
    }

    #[test]
    fn unload_equals_composing_without(
        sets in proptest::collection::vec(
            (0..KINDS.len(), 0..2usize, proptest::collection::vec((0u8..4, 0u8..6), 0..4)), 0..9),
        gone in 0..3usize,
    ) {
        let snap = snapshots();
        let (mut c, mut q) = (Contributions::new(), RequestQueues::default());
        for (i, (k, scope, recs)) in sets.into_iter().enumerate() {
            let r = Rec::Contribute { kind: KINDS[k], scope, recs };
            feed(PLUGINS[i % 3], &[r], &snap, &mut c, &mut q);
        }
        let without: Vec<PluginId> = PLUGINS.iter().copied().filter(|&p| p != PLUGINS[gone]).collect();
        let expected = compose(&c, &without);
        let mut unloaded = c.clone();
        unloaded.remove_plugin(PLUGINS[gone]);
        prop_assert_eq!(compose(&unloaded, &without), expected.clone());
        prop_assert_eq!(compose(&unloaded, &PLUGINS), expected, "an unloaded plugin's order slot is inert");
    }

    #[test]
    fn composition_is_idempotent(
        sets in proptest::collection::vec(
            (0..KINDS.len(), 0..2usize, proptest::collection::vec((0u8..4, 0u8..6), 0..4)), 0..9),
    ) {
        let snap = snapshots();
        let (mut c, mut q) = (Contributions::new(), RequestQueues::default());
        let recs: Vec<(PluginId, Rec)> = sets
            .into_iter()
            .enumerate()
            .map(|(i, (k, scope, recs))| (PLUGINS[i % 3], Rec::Contribute { kind: KINDS[k], scope, recs }))
            .collect();
        for (p, r) in &recs {
            feed(*p, std::slice::from_ref(r), &snap, &mut c, &mut q);
        }
        let once = compose(&c, &PLUGINS);
        prop_assert_eq!(compose(&c, &PLUGINS), once.clone(), "deterministic");

        // Re-declaring the current sets (immediate mode) changes nothing.
        let (epoch, before) = (c.epoch(), c.clone());
        for (p, _) in &recs {
            for k in KINDS {
                for scope in ["", SCOPES[0], SCOPES[1]] {
                    if let Some(body) = before.get(*p, k, scope) {
                        let mut o = Out::new();
                        if k.scoped() { o.contribute_scoped(k, scope, body) } else { o.contribute(k, body) }
                        let mut rej = Vec::new();
                        intake(*p, o.as_str().as_bytes(), &snap, &mut c, &mut q, &mut rej);
                        prop_assert!(rej.is_empty());
                    }
                }
            }
        }
        prop_assert_eq!(c.epoch(), epoch);
        prop_assert_eq!(compose(&c, &PLUGINS), once.clone());

        // Composing the composition (as one plugin's sets) is a fixed point.
        let mut again = Contributions::new();
        let solo = PluginId(9);
        let mut groups: BTreeMap<(Kind, &str), Vec<&str>> = BTreeMap::new();
        for r in &once.0.records {
            groups.entry((r.kind, &*r.scope)).or_default().push(&*r.text);
        }
        for ((k, scope), texts) in groups {
            let mut o = Out::new();
            let body = texts.join("\n");
            if k.scoped() { o.contribute_scoped(k, scope, &body) } else { o.contribute(k, &body) }
            let mut rej = Vec::new();
            intake(solo, o.as_str().as_bytes(), &snap, &mut again, &mut q, &mut rej);
            prop_assert!(rej.is_empty());
        }
        let (fixed, overrides) = compose(&again, &[solo]);
        let strip = |v: Vec<Row>| -> Vec<_> {
            v.into_iter().map(|(k, s, key, _, t)| (k, s, key, t)).collect()
        };
        prop_assert_eq!(strip(flatten(&fixed)), strip(flatten(&once.0)));
        prop_assert!(overrides.is_empty());
    }

    #[test]
    fn random_output_bytes_only_declared_errors(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let snap = snapshots();
        let (mut c, mut q, mut rej) = (Contributions::new(), RequestQueues::default(), Vec::new());
        let accepted = intake(PLUGINS[0], &bytes, &snap, &mut c, &mut q, &mut rej);
        prop_assert_eq!(accepted as usize + rej.len(), text::chunks(&bytes).count());
        prop_assert!(rej.iter().all(|r| matches!(r.code, Code::Parse | Code::Gone | Code::Range | Code::Space)));
        let _ = compose(&c, &PLUGINS);
    }

    #[test]
    fn random_record_text_only_declared_errors(s in "([a-z_]{1,10}( [a-z0-9\"|#:.\\\\-]{0,6}){0,4}\n(  [|a-z \"]{0,10}\n){0,3}){1,6}") {
        let snap = snapshots();
        let (mut c, mut q, mut rej) = (Contributions::new(), RequestQueues::default(), Vec::new());
        let accepted = intake(PLUGINS[0], s.as_bytes(), &snap, &mut c, &mut q, &mut rej);
        prop_assert_eq!(accepted as usize + rej.len(), text::chunks(s.as_bytes()).count());
        prop_assert!(rej.iter().all(|r| matches!(r.code, Code::Parse | Code::Gone | Code::Range | Code::Space)));
        let _ = compose(&c, &PLUGINS);
    }

    #[test]
    fn capability_decisions_follow_the_grants(
        grants in proptest::sample::subsequence(
            vec!["read_project", "write_project", "network", "clipboard", "spawn:ngspice", "spawn:xyce"], 0..=6),
        op in proptest::sample::select(vec![
            IoOp::FsRead, IoOp::FsWrite, IoOp::FsRemove, IoOp::Spawn, IoOp::Fetch, IoOp::ClipboardGet, IoOp::ClipboardSet,
        ]),
        args in proptest::collection::vec(prop_oneof![
            proptest::sample::select(vec![
                "lib/a.shcomp", "../x", "/abs", "a/./b", "a//b", "c:x", "ngspice", "xyce", "sh", "", "https://x",
                ".schemify/pdks/a", ".schemify", ".schemify/../x", ".schemify/schemify.lock", "schemify.project",
            ]).prop_map(String::from),
            "\\PC{0,8}",
        ], 0..3),
    ) {
        let g = Grants::parse(&grants).unwrap();
        let got = authorize(&g, op, &args);
        prop_assert!(matches!(got, Ok(()) | Err(Code::Denied) | Err(Code::Parse)));
        let arity = match op {
            IoOp::FsRead | IoOp::FsWrite | IoOp::FsRemove | IoOp::Fetch => args.len() == 1,
            IoOp::Spawn => !args.is_empty(),
            IoOp::ClipboardGet | IoOp::ClipboardSet => args.is_empty(),
        };
        if !arity {
            prop_assert_eq!(got, Err(Code::Parse));
            return Ok(());
        }
        // Positive and negative space: allowed exactly when the spec's rule says so.
        let clean = |p: &str| {
            !p.is_empty() && !p.starts_with('/') && !p.contains(['\\', ':', '\0'])
                && p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
        };
        let allowed = g.has(op.cap()) && match op {
            IoOp::FsRead | IoOp::FsWrite => clean(&args[0]) && args[0].len() <= 4096,
            IoOp::FsRemove => clean(&args[0]) && args[0].len() <= 4096
                && args[0].split('/').count() >= 2 && args[0].split('/').next() == Some(".schemify")
                && !matches!(args[0].rsplit('/').next(), Some("schemify.project" | "schemify.lock")),
            IoOp::Spawn => grants.iter().any(|gr| gr.strip_prefix("spawn:") == Some(args[0].as_str())),
            IoOp::Fetch => !args[0].is_empty(),
            IoOp::ClipboardGet | IoOp::ClipboardSet => true,
        };
        prop_assert_eq!(got.is_ok(), allowed, "{:?} {:?} {:?}", grants, op, args);
    }
}

/// Takes `body` in as `p`'s `ui` set.
fn ui(p: PluginId, body: &str, c: &mut Contributions) {
    let mut o = Out::new();
    o.contribute(Kind::Ui, body);
    let (mut q, mut rej) = (RequestQueues::default(), Vec::new());
    intake(p, o.as_str().as_bytes(), &snapshots(), c, &mut q, &mut rej);
    assert!(rej.is_empty(), "{rej:?}");
}

/// A later plugin's full redefinition of a node (`node X`, `remove X`) drops every earlier
/// `set X …`, as the UI composition does; a later set applies again.
#[test]
fn a_full_redefinition_drops_earlier_sets() {
    let [a, b, c3] = PLUGINS;
    let mut c = Contributions::new();
    ui(
        a,
        "node x item\n  label one\nset x label two\nset y label keep",
        &mut c,
    );
    ui(b, "node x item\n  label three", &mut c);
    let keys = |composed: &Composed| -> Vec<String> {
        composed.records.iter().map(|r| r.key.to_string()).collect()
    };
    let (composed, overrides) = compose(&c, &PLUGINS[..2]);
    assert_eq!(keys(&composed), ["node x", "set y label"]);
    assert_eq!(composed.records[0].owner, b);
    let dropped = overrides
        .iter()
        .filter(|o| &*o.key == "set x label")
        .map(|o| (o.loser, o.winner));
    assert_eq!(dropped.collect::<Vec<_>>(), [(a, b)]);

    ui(c3, "set x label four", &mut c);
    let (composed, _) = compose(&c, &PLUGINS);
    assert_eq!(keys(&composed), ["node x", "set y label", "set x label"]);
    assert_eq!(composed.records[2].owner, c3);

    // `remove` is a redefinition too, and it drops the same plugin's earlier set.
    ui(c3, "set x label four\nremove x", &mut c);
    let (composed, _) = compose(&c, &PLUGINS);
    assert_eq!(keys(&composed), ["node x", "set y label"]);
    assert_eq!(&*composed.records[0].text, "remove x");
}

/// Composition key of a set member's first line, computed like the spec says (word and first
/// value; `remove` shares `node`; `set` adds the property).
fn line_key(line: &str) -> String {
    let (word, rest) = text::split_word(line).unwrap();
    let mut v = text::values(rest);
    let first = v.next().unwrap().unwrap();
    match word {
        "remove" => format!("node {first}"),
        "set" => format!("set {first} {}", v.next().unwrap().unwrap()),
        w => format!("{w} {first}"),
    }
}
