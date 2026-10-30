//! Properties of the host's pure transforms (docs/dev/testing.md `plugin` row), no wasm involved:
//! stateful fuzz of output records against a naive model (malformed streams, stale
//! documents, conflicting contributions, full queues) and composition laws (last wins,
//! unload == compose without, idempotence). Cites plugin-abi §1 (composition), §7, §8.

use std::collections::BTreeMap;

use std::slice;

use proptest::prelude::*;
use schemify_plugin as host;
use schemify_plugin::abi::{Code, EditMode, Kind, Out, Table, col, text};
use schemify_plugin::{
    Composed, Contributions, EDITS_MAX, LOGS_MAX, Override, PluginId, RUNS_MAX, RequestQueues,
    Snapshots, TIMER_MS_MAX, TIMERS_MAX,
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

/// A composed record with owned text, as the reference composition derives it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    kind: Kind,
    scope: String,
    key: String,
    owner: PluginId,
    text: String,
}

/// One set member: its record text and its composition key.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Member {
    text: String,
    key: String,
}

/// Where a set lives in the model: its plugin, kind and scope.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SetKey {
    plugin: PluginId,
    kind: Kind,
    scope: String,
}

/// One generated output record; the generator knows whether it is valid.
#[derive(Clone, Debug)]
enum Record {
    /// A whole set: (name index, variant) per member. Always valid.
    Contribute {
        kind: Kind,
        scope: usize,
        members: Vec<(u8, u8)>,
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

#[derive(Clone, Debug)]
enum Step {
    Output { plugin: usize, records: Vec<Record> },
    Unload(usize),
}

/// The naive model: current sets as member lists, and queue lengths.
#[derive(Default)]
struct Model {
    sets: BTreeMap<SetKey, Vec<Member>>,
    edits: usize,
    runs: usize,
    timers: usize,
    logs: usize,
}

// ---- private ----

/// One set member, its key derived independently of the host.
fn member(kind: Kind, name: u8, variant: u8) -> Member {
    let word = ["a", "b", "c", "d"][name as usize % 4];
    let (text, key) = match kind {
        Kind::Ui => match variant % 3 {
            0 => (
                format!("node {word} item\n  label \"{word}\""),
                format!("node {word}"),
            ),
            1 => (format!("remove {word}"), format!("node {word}")),
            _ => (
                format!("set {word} label x{variant}"),
                format!("set {word} label"),
            ),
        },
        Kind::Commands => (
            format!("command cmd.{word} \"L{variant}\""),
            format!("command cmd.{word}"),
        ),
        Kind::Keymap => (
            format!("key ctrl+{word} cmd.{word}{variant}"),
            format!("key ctrl+{word}"),
        ),
        Kind::Annotations => (
            format!("mark {word} warn \"m{variant}\""),
            format!("mark {word}"),
        ),
        Kind::Functions => (
            format!("function ns.{word} {variant} 2"),
            format!("function ns.{word}"),
        ),
        _ => unreachable!("not generated"),
    };
    Member { text, key }
}

fn scope_of(kind: Kind, scope: usize) -> &'static str {
    if kind.scoped() { SCOPES[scope % 2] } else { "" }
}

fn render(record: &Record, bytes: &mut Vec<u8>) {
    let mut out = Out::new();
    match record {
        Record::Contribute {
            kind,
            scope,
            members,
        } => {
            let texts: Vec<String> = members
                .iter()
                .map(|&(name, variant)| member(*kind, name, variant).text)
                .collect();
            let body = texts.join("\n");
            if kind.scoped() {
                out.contribute_scoped(*kind, scope_of(*kind, *scope), &body);
            } else {
                out.contribute(*kind, &body);
            }
        }
        Record::Edit { doc } => out.edit(*doc, EditMode::Stage, "size", "set_attr M1 w 2u"),
        Record::Run => out.run("lib:tb", "default", "tt", &[("vdd", "1.8")]),
        Record::Timer { ms } => out.timer(7, *ms),
        Record::Log(text) => out.log(text),
        Record::Garbage(garbage) => {
            bytes.push(0xff);
            bytes.extend(garbage.iter().filter(|&&b| b != b'\n'));
            bytes.push(b'\n');
        }
        Record::NearMiss(i) => {
            bytes.extend_from_slice(NEAR_MISSES[*i].as_bytes());
            bytes.push(b'\n');
        }
    }
    bytes.extend_from_slice(out.as_str().as_bytes());
}

fn record() -> impl Strategy<Value = Record> {
    prop_oneof![
        4 => (0..KINDS.len(), 0..2usize, proptest::collection::vec((0u8..4, 0u8..6), 0..4))
            .prop_map(|(k, scope, members)| Record::Contribute { kind: KINDS[k], scope, members }),
        1 => prop_oneof![Just(LIVE_DOC), Just(2u32), any::<u32>()].prop_map(|doc| Record::Edit { doc }),
        1 => Just(Record::Run),
        1 => prop_oneof![0..=TIMER_MS_MAX, Just(TIMER_MS_MAX + 1), any::<u32>()].prop_map(|ms| Record::Timer { ms }),
        1 => "\\PC{0,12}".prop_map(Record::Log),
        1 => proptest::collection::vec(any::<u8>(), 0..16).prop_map(Record::Garbage),
        1 => (0..NEAR_MISSES.len()).prop_map(Record::NearMiss),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        8 => (0..3usize, proptest::collection::vec(record(), 0..10)).prop_map(|(plugin, records)| Step::Output { plugin, records }),
        1 => (0..3usize).prop_map(Step::Unload),
    ]
}

fn snapshots() -> Snapshots {
    let mut tables = Snapshots::new();
    tables.fill::<u32>(Table::Docs, col::docs::HANDLE, &[LIVE_DOC]);
    tables.fill::<u8>(Table::Docs, col::docs::KIND, &[1]);
    tables.fill::<u8>(Table::Docs, col::docs::DIRTY, &[0]);
    tables.fill_strings(Table::Docs, col::docs::PATH, &["top.shcomp"]);
    tables.fill_strings(Table::Docs, col::docs::CELL, &["top"]);
    tables
}

impl Model {
    /// Applies one record; returns the code the host must reject it with, if any.
    fn apply(&mut self, plugin: PluginId, record: &Record) -> Option<Code> {
        let bounded = |n: &mut usize, max: usize| {
            if *n >= max {
                Some(Code::Space)
            } else {
                *n += 1;
                None
            }
        };
        match record {
            Record::Contribute {
                kind,
                scope,
                members,
            } => {
                let key = SetKey {
                    plugin,
                    kind: *kind,
                    scope: scope_of(*kind, *scope).to_owned(),
                };
                let set: Vec<Member> = members
                    .iter()
                    .map(|&(name, variant)| member(*kind, name, variant))
                    .collect();
                if set.is_empty() {
                    self.sets.remove(&key);
                } else {
                    self.sets.insert(key, set);
                }
                None
            }
            Record::Edit { doc } if *doc != LIVE_DOC => Some(Code::Gone),
            Record::Edit { .. } => bounded(&mut self.edits, EDITS_MAX),
            Record::Run => bounded(&mut self.runs, RUNS_MAX),
            Record::Timer { ms } if *ms > TIMER_MS_MAX => Some(Code::Range),
            Record::Timer { .. } => bounded(&mut self.timers, TIMERS_MAX),
            Record::Log(_) => bounded(&mut self.logs, LOGS_MAX),
            Record::Garbage(_) | Record::NearMiss(_) => Some(Code::Parse),
        }
    }

    /// Reference composition: for every key the last plugin in `order` wins; keys keep
    /// their first-appearance order per (kind, scope); a `node X` / `remove X` drops every
    /// `set X …` before it (a set after it appears again, at the end).
    fn compose(&self, order: &[PluginId]) -> Vec<Row> {
        let mut groups: BTreeMap<(Kind, String), Vec<Row>> = BTreeMap::new();
        for &plugin in order {
            for (set, members) in &self.sets {
                if set.plugin != plugin {
                    continue;
                }
                let group = groups.entry((set.kind, set.scope.clone())).or_default();
                for Member { text, key } in members {
                    if let Some(node) = key.strip_prefix("node ") {
                        let prefix = format!("set {node} ");
                        group.retain(|row| !row.key.starts_with(&prefix));
                    }
                    match group.iter_mut().find(|row| row.key == *key) {
                        Some(row) => (row.owner, row.text) = (plugin, text.clone()),
                        None => group.push(Row {
                            kind: set.kind,
                            scope: set.scope.clone(),
                            key: key.clone(),
                            owner: plugin,
                            text: text.clone(),
                        }),
                    }
                }
            }
        }
        groups.into_values().flatten().collect()
    }
}

fn flatten(composed: &Composed) -> Vec<Row> {
    composed
        .records
        .iter()
        .map(|record| Row {
            kind: record.kind,
            scope: record.scope.to_string(),
            key: record.key.to_string(),
            owner: record.owner,
            text: record.text.to_string(),
        })
        .collect()
}

fn feed(
    plugin: PluginId,
    records: &[Record],
    tables: &Snapshots,
    contributions: &mut Contributions,
    queues: &mut RequestQueues,
) -> Vec<Code> {
    let mut bytes = Vec::new();
    for record in records {
        render(record, &mut bytes);
    }
    let mut rejections = Vec::new();
    let accepted = host::intake(
        plugin,
        &bytes,
        tables,
        contributions,
        queues,
        &mut rejections,
    );
    assert_eq!(
        accepted as usize + rejections.len(),
        records.len(),
        "one chunk per record"
    );
    for rejection in &rejections {
        assert_eq!(rejection.plugin, plugin);
        assert!(
            rejection.record.len() <= 256,
            "plugin-abi §8 echoes at most 256 bytes"
        );
    }
    rejections.iter().map(|rejection| rejection.code).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn intake_and_compose_match_the_model(steps in proptest::collection::vec(step(), 1..16)) {
        let tables = snapshots();
        let mut contributions = Contributions::new();
        let mut queues = RequestQueues::default();
        let mut model = Model::default();
        let mut live = PLUGINS.to_vec();
        for step in &steps {
            match step {
                Step::Unload(i) => {
                    contributions.remove_plugin(PLUGINS[*i]);
                    model.sets.retain(|set, _| set.plugin != PLUGINS[*i]);
                    live.retain(|&plugin| plugin != PLUGINS[*i]);
                }
                Step::Output { plugin, records } => {
                    let plugin = PLUGINS[*plugin];
                    if !live.contains(&plugin) {
                        continue;
                    }
                    let got = feed(plugin, records, &tables, &mut contributions, &mut queues);
                    let want: Vec<Code> = records
                        .iter()
                        .filter_map(|record| model.apply(plugin, record))
                        .collect();
                    prop_assert_eq!(got, want);
                }
            }
            prop_assert_eq!(contributions.len(), model.sets.len());
            for (set, members) in &model.sets {
                let texts: Vec<&str> = members.iter().map(|member| member.text.as_str()).collect();
                let body = texts.join("\n");
                prop_assert_eq!(contributions.get(set.plugin, set.kind, &set.scope), Some(body.as_str()));
            }
            prop_assert_eq!(queues.edits.len(), model.edits);
            prop_assert_eq!(queues.runs.len(), model.runs);
            prop_assert_eq!(queues.timers.len(), model.timers);
            prop_assert_eq!(queues.logs.len(), model.logs);
            let (composed, overrides) = host::compose(&contributions, &live);
            let reference = model.compose(&live);
            prop_assert_eq!(flatten(&composed), reference.clone());
            let live_sets = model.sets.iter().filter(|(set, _)| live.contains(&set.plugin));
            let total: usize = live_sets.map(|(_, members)| members.len()).sum();
            prop_assert_eq!(overrides.len(), total - reference.len(), "one override per replaced record");
            let mut names: Vec<String> = reference
                .iter()
                .filter(|row| row.kind == Kind::Functions)
                .map(|row| row.key.trim_start_matches("function ").to_owned())
                .collect();
            names.sort();
            let registry: Vec<String> = composed.functions.name.iter().map(|name| name.to_string()).collect();
            prop_assert_eq!(registry, names);
        }
    }

    #[test]
    fn last_plugin_in_order_wins(
        sets in proptest::collection::vec(proptest::collection::vec((0u8..4, 0u8..6), 1..5), 3),
        order in Just(PLUGINS.to_vec()).prop_shuffle(),
    ) {
        let tables = snapshots();
        let mut contributions = Contributions::new();
        let mut queues = RequestQueues::default();
        let mut model = Model::default();
        for (plugin, members) in PLUGINS.iter().zip(&sets) {
            let record = Record::Contribute { kind: Kind::Ui, scope: 0, members: members.clone() };
            model.apply(*plugin, &record);
            let rejected = feed(*plugin, &[record], &tables, &mut contributions, &mut queues);
            prop_assert!(rejected.is_empty());
        }
        let (composed, overrides) = host::compose(&contributions, &order);
        prop_assert_eq!(flatten(&composed), model.compose(&order));
        for record in &composed.records {
            let has_key = |body: &str| {
                body.lines().any(|line| !line.starts_with(' ') && line_key(line) == *record.key)
            };
            let last = order
                .iter()
                .rev()
                .find(|&&plugin| contributions.get(plugin, Kind::Ui, "").is_some_and(has_key));
            prop_assert_eq!(Some(&record.owner), last);
        }
        let rank = |plugin: PluginId| order.iter().position(|&ranked| ranked == plugin);
        let ordered = overrides.iter().all(|o: &Override| rank(o.loser) <= rank(o.winner));
        prop_assert!(ordered, "an override never goes backwards in plugin order");
    }

    #[test]
    fn unload_equals_composing_without(
        sets in proptest::collection::vec(
            (0..KINDS.len(), 0..2usize, proptest::collection::vec((0u8..4, 0u8..6), 0..4)), 0..9),
        gone in 0..3usize,
    ) {
        let tables = snapshots();
        let mut contributions = Contributions::new();
        let mut queues = RequestQueues::default();
        for (i, (k, scope, members)) in sets.into_iter().enumerate() {
            let record = Record::Contribute { kind: KINDS[k], scope, members };
            feed(PLUGINS[i % 3], &[record], &tables, &mut contributions, &mut queues);
        }
        let without: Vec<PluginId> = PLUGINS.iter().copied().filter(|&p| p != PLUGINS[gone]).collect();
        let expected = host::compose(&contributions, &without);
        let mut unloaded = contributions.clone();
        unloaded.remove_plugin(PLUGINS[gone]);
        prop_assert_eq!(host::compose(&unloaded, &without), expected.clone());
        prop_assert_eq!(host::compose(&unloaded, &PLUGINS), expected, "an unloaded plugin's order slot is inert");
    }

    #[test]
    fn composition_is_idempotent(
        sets in proptest::collection::vec(
            (0..KINDS.len(), 0..2usize, proptest::collection::vec((0u8..4, 0u8..6), 0..4)), 0..9),
    ) {
        let tables = snapshots();
        let mut contributions = Contributions::new();
        let mut queues = RequestQueues::default();
        let mut fed: Vec<PluginId> = Vec::new();
        for (i, (k, scope, members)) in sets.into_iter().enumerate() {
            let plugin = PLUGINS[i % 3];
            let record = Record::Contribute { kind: KINDS[k], scope, members };
            feed(plugin, slice::from_ref(&record), &tables, &mut contributions, &mut queues);
            fed.push(plugin);
        }
        let once = host::compose(&contributions, &PLUGINS);
        prop_assert_eq!(host::compose(&contributions, &PLUGINS), once.clone(), "deterministic");

        // Re-declaring the current sets (immediate mode) changes nothing.
        let (epoch, before) = (contributions.epoch(), contributions.clone());
        for &plugin in &fed {
            for kind in KINDS {
                for scope in ["", SCOPES[0], SCOPES[1]] {
                    if let Some(body) = before.get(plugin, kind, scope) {
                        let mut out = Out::new();
                        if kind.scoped() {
                            out.contribute_scoped(kind, scope, body);
                        } else {
                            out.contribute(kind, body);
                        }
                        let mut rejections = Vec::new();
                        let bytes = out.as_str().as_bytes();
                        host::intake(plugin, bytes, &tables, &mut contributions, &mut queues, &mut rejections);
                        prop_assert!(rejections.is_empty());
                    }
                }
            }
        }
        prop_assert_eq!(contributions.epoch(), epoch);
        prop_assert_eq!(host::compose(&contributions, &PLUGINS), once.clone());

        // Composing the composition (as one plugin's sets) is a fixed point.
        let mut again = Contributions::new();
        let solo = PluginId(9);
        let mut groups: BTreeMap<(Kind, &str), Vec<&str>> = BTreeMap::new();
        for record in &once.0.records {
            groups.entry((record.kind, &*record.scope)).or_default().push(&*record.text);
        }
        for ((kind, scope), texts) in groups {
            let mut out = Out::new();
            let body = texts.join("\n");
            if kind.scoped() {
                out.contribute_scoped(kind, scope, &body);
            } else {
                out.contribute(kind, &body);
            }
            let mut rejections = Vec::new();
            let bytes = out.as_str().as_bytes();
            host::intake(solo, bytes, &tables, &mut again, &mut queues, &mut rejections);
            prop_assert!(rejections.is_empty());
        }
        let (fixed, overrides) = host::compose(&again, &[solo]);
        // Every row of `fixed` is owned by `solo`: compare all but the owner.
        let strip = |rows: Vec<Row>| -> Vec<Row> {
            rows.into_iter().map(|row| Row { owner: solo, ..row }).collect()
        };
        prop_assert_eq!(strip(flatten(&fixed)), strip(flatten(&once.0)));
        prop_assert!(overrides.is_empty());
    }

    #[test]
    fn random_output_bytes_only_declared_errors(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let tables = snapshots();
        let mut contributions = Contributions::new();
        let (mut queues, mut rejections) = (RequestQueues::default(), Vec::new());
        let accepted = host::intake(PLUGINS[0], &bytes, &tables, &mut contributions, &mut queues, &mut rejections);
        prop_assert_eq!(accepted as usize + rejections.len(), text::chunks(&bytes).count());
        let declared = |code| matches!(code, Code::Parse | Code::Gone | Code::Range | Code::Space);
        prop_assert!(rejections.iter().all(|rejection| declared(rejection.code)));
        let _ = host::compose(&contributions, &PLUGINS);
    }

    #[test]
    fn random_record_text_only_declared_errors(source in "([a-z_]{1,10}( [a-z0-9\"|#:.\\\\-]{0,6}){0,4}\n(  [|a-z \"]{0,10}\n){0,3}){1,6}") {
        let tables = snapshots();
        let mut contributions = Contributions::new();
        let (mut queues, mut rejections) = (RequestQueues::default(), Vec::new());
        let bytes = source.as_bytes();
        let accepted = host::intake(PLUGINS[0], bytes, &tables, &mut contributions, &mut queues, &mut rejections);
        prop_assert_eq!(accepted as usize + rejections.len(), text::chunks(bytes).count());
        let declared = |code| matches!(code, Code::Parse | Code::Gone | Code::Range | Code::Space);
        prop_assert!(rejections.iter().all(|rejection| declared(rejection.code)));
        let _ = host::compose(&contributions, &PLUGINS);
    }
}

/// Takes `body` in as `plugin`'s `ui` set.
fn ui(plugin: PluginId, body: &str, contributions: &mut Contributions) {
    let mut out = Out::new();
    out.contribute(Kind::Ui, body);
    let (mut queues, mut rejections) = (RequestQueues::default(), Vec::new());
    let bytes = out.as_str().as_bytes();
    host::intake(
        plugin,
        bytes,
        &snapshots(),
        contributions,
        &mut queues,
        &mut rejections,
    );
    assert!(rejections.is_empty(), "{rejections:?}");
}

/// A later plugin's full redefinition of a node (`node X`, `remove X`) drops every earlier
/// `set X …`, as the UI composition does; a later set applies again.
#[test]
fn a_full_redefinition_drops_earlier_sets() {
    let [a, b, c] = PLUGINS;
    let mut contributions = Contributions::new();
    ui(
        a,
        "node x item\n  label one\nset x label two\nset y label keep",
        &mut contributions,
    );
    ui(b, "node x item\n  label three", &mut contributions);
    let keys = |composed: &Composed| -> Vec<String> {
        composed
            .records
            .iter()
            .map(|record| record.key.to_string())
            .collect()
    };
    let (composed, overrides) = host::compose(&contributions, &PLUGINS[..2]);
    assert_eq!(keys(&composed), ["node x", "set y label"]);
    assert_eq!(composed.records[0].owner, b);
    let dropped: Vec<(PluginId, PluginId)> = overrides
        .iter()
        .filter(|o| &*o.key == "set x label")
        .map(|o| (o.loser, o.winner))
        .collect();
    assert_eq!(dropped, [(a, b)]);

    ui(c, "set x label four", &mut contributions);
    let (composed, _) = host::compose(&contributions, &PLUGINS);
    assert_eq!(keys(&composed), ["node x", "set y label", "set x label"]);
    assert_eq!(composed.records[2].owner, c);

    // `remove` is a redefinition too, and it drops the same plugin's earlier set.
    ui(c, "set x label four\nremove x", &mut contributions);
    let (composed, _) = host::compose(&contributions, &PLUGINS);
    assert_eq!(keys(&composed), ["node x", "set y label"]);
    assert_eq!(&*composed.records[0].text, "remove x");
}

/// Composition key of a set member's first line, computed like the spec says (word and first
/// value; `remove` shares `node`; `set` adds the property).
fn line_key(line: &str) -> String {
    let (word, rest) = text::split_word(line).expect("a generated member starts with its word");
    let mut values = text::values(rest);
    let mut next_value = || {
        let value = values
            .next()
            .expect("a generated member names its key values");
        value.expect("generated values lex")
    };
    let first = next_value();
    match word {
        "remove" => format!("node {first}"),
        "set" => format!("set {first} {}", next_value()),
        other => format!("{other} {first}"),
    }
}
