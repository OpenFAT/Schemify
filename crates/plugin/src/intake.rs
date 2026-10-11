//! Output intake (§7, §8): one plugin's output bytes → contribution sets, request queues
//! and rejections. Pure over its parameters, so it is fuzzed without any wasm.

use schemify_abi::text;
use schemify_abi::{Code, Kind, Output};
use schemify_format::Document as FormatDoc;

use crate::{
    CONTRIBUTION_BYTES_MAX, CONTRIBUTIONS_MAX, EDITS_MAX, EditRequest, Grants, IoOp, LOG_BYTES_MAX,
    LOGS_MAX, LogEntry, PluginId, REJECT_ECHO_BYTES, RUNS_MAX, RequestQueues, RunRequest,
    Snapshots, TIMER_MS_MAX, TIMERS_MAX, TimerRequest,
};

/// Every plugin's current contribution set per (kind, scope); an empty set has no row
/// (existence-based). Capacity [`CONTRIBUTIONS_MAX`] rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contributions {
    plugin: Vec<PluginId>,
    kind: Vec<Kind>,
    scope: Vec<Box<str>>,
    body: Vec<Box<str>>,
    epoch: u32,
}

impl Contributions {
    /// Returns an empty table.
    pub fn new() -> Contributions {
        Contributions::default()
    }

    /// Returns the number of non-empty sets.
    pub fn len(&self) -> usize {
        self.plugin.len()
    }

    /// Returns whether no plugin contributes anything.
    pub fn is_empty(&self) -> bool {
        self.plugin.is_empty()
    }

    /// Returns a value that changes whenever any set changes.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Returns the current set of `plugin` for (`kind`, `scope`), or `None` when empty.
    pub fn get(&self, plugin: PluginId, kind: Kind, scope: &str) -> Option<&str> {
        self.find(plugin, kind, scope).map(|i| &*self.body[i])
    }

    /// Iterates every row as (plugin, kind, scope, body), in insertion order.
    pub fn rows(&self) -> impl Iterator<Item = (PluginId, Kind, &str, &str)> {
        (0..self.len()).map(|i| {
            (
                self.plugin[i],
                self.kind[i],
                &*self.scope[i],
                &*self.body[i],
            )
        })
    }

    /// Drops every set of `plugin` (unload).
    pub fn remove_plugin(&mut self, plugin: PluginId) {
        let before = self.len();
        let mut i = 0;
        while i < self.plugin.len() {
            if self.plugin[i] == plugin {
                self.remove(i);
            } else {
                i += 1;
            }
        }
        if self.len() != before {
            self.epoch = self.epoch.wrapping_add(1);
        }
        debug_assert!(self.plugin.iter().all(|&p| p != plugin));
    }

    /// Replaces a set (replace semantics); an empty `body` removes the row.
    fn set(&mut self, plugin: PluginId, kind: Kind, scope: &str, body: &str) -> Result<(), Code> {
        debug_assert!(kind.scoped() || scope.is_empty());
        match (self.find(plugin, kind, scope), body.is_empty()) {
            (Some(i), true) => self.remove(i),
            (Some(i), false) if *self.body[i] == *body => return Ok(()),
            (Some(i), false) => self.body[i] = body.into(),
            (None, true) => return Ok(()),
            (None, false) => {
                if self.len() >= CONTRIBUTIONS_MAX {
                    return Err(Code::Space);
                }
                self.plugin.push(plugin);
                self.kind.push(kind);
                self.scope.push(scope.into());
                self.body.push(body.into());
            }
        }
        self.epoch = self.epoch.wrapping_add(1);
        debug_assert!(self.len() <= CONTRIBUTIONS_MAX);
        Ok(())
    }

    fn find(&self, plugin: PluginId, kind: Kind, scope: &str) -> Option<usize> {
        (0..self.len())
            .find(|&i| self.plugin[i] == plugin && self.kind[i] == kind && *self.scope[i] == *scope)
    }

    fn remove(&mut self, i: usize) {
        self.plugin.remove(i);
        self.kind.remove(i);
        self.scope.remove(i);
        self.body.remove(i);
    }
}

/// One rejected output record (§8). Goes to the health report and, unless it arose while
/// handling a rejection, back to the plugin as `Event::Rejected`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    /// Producing plugin.
    pub plugin: PluginId,
    /// Why.
    pub code: Code,
    /// The record, truncated to [`REJECT_ECHO_BYTES`].
    pub record: Box<[u8]>,
}

/// Takes one plugin's output in: each record is accepted or rejected on its own.
/// Contributions replace the plugin's set; requests are appended to `queues`; rejections
/// are appended to `rejections`. Returns the number of accepted records.
///
/// Rejection codes: `E_PARSE` (invalid UTF-8, malformed or unknown record, record word not
/// legal for the contribution kind), `E_GONE` (`edit` of a document not in
/// `snapshots`' `Docs`), `E_RANGE` (timer over [`TIMER_MS_MAX`]), `E_SPACE` (queue or
/// contribution table full, set over [`CONTRIBUTION_BYTES_MAX`]).
pub fn intake(
    plugin: PluginId,
    output: &[u8],
    snapshots: &Snapshots,
    contributions: &mut Contributions,
    queues: &mut RequestQueues,
    rejections: &mut Vec<Rejection>,
) -> u32 {
    let rejected_before = rejections.len();
    let mut accepted = 0u32;
    for chunk in text::chunks(output) {
        match accept(plugin, chunk, snapshots, contributions, queues) {
            Ok(()) => accepted += 1,
            Err(code) => rejections.push(Rejection {
                plugin,
                code,
                record: chunk[..chunk.len().min(REJECT_ECHO_BYTES)].into(),
            }),
        }
    }
    debug_assert_eq!(
        accepted as usize + rejections.len() - rejected_before,
        text::chunks(output).count()
    );
    accepted
}

fn accept(
    plugin: PluginId,
    chunk: &[u8],
    snapshots: &Snapshots,
    contributions: &mut Contributions,
    queues: &mut RequestQueues,
) -> Result<(), Code> {
    let chunk = std::str::from_utf8(chunk).map_err(|_| Code::Parse)?;
    match Output::parse(chunk)? {
        Output::Contribute { kind, scope, body } => {
            if body.len() > CONTRIBUTION_BYTES_MAX {
                return Err(Code::Space);
            }
            set_records(kind, &body)?;
            contributions.set(plugin, kind, &scope, &body)
        }
        Output::Edit {
            doc,
            mode,
            label,
            ops,
        } => {
            if !snapshots.has_doc(doc) {
                return Err(Code::Gone);
            }
            let req = EditRequest {
                plugin,
                doc,
                mode,
                label: label.into_owned(),
                ops,
            };
            push_bounded(&mut queues.edits, EDITS_MAX, req)
        }
        Output::Run {
            bench,
            config,
            corner,
            vary,
        } => {
            let vary = vary
                .into_iter()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let req = RunRequest {
                plugin,
                bench: bench.into_owned(),
                config: config.into_owned(),
                corner: corner.into_owned(),
                vary,
            };
            push_bounded(&mut queues.runs, RUNS_MAX, req)
        }
        Output::Timer { id, ms } => {
            if ms > TIMER_MS_MAX {
                return Err(Code::Range);
            }
            push_bounded(
                &mut queues.timers,
                TIMERS_MAX,
                TimerRequest { plugin, id, ms },
            )
        }
        Output::Log { level, text } => {
            let text = truncate(&text, LOG_BYTES_MAX).to_owned();
            push_bounded(
                &mut queues.logs,
                LOGS_MAX,
                LogEntry {
                    plugin,
                    level,
                    text,
                },
            )
        }
    }
}

fn push_bounded<T>(queue: &mut Vec<T>, max: usize, item: T) -> Result<(), Code> {
    if queue.len() >= max {
        return Err(Code::Space);
    }
    queue.push(item);
    Ok(())
}

/// Returns the longest prefix of `s` of at most `max` bytes that ends on a char boundary.
pub(crate) fn truncate(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// One top-level record of a contribution set: its composition key and its full text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetRecord<'a> {
    pub key: String,
    pub text: &'a str,
}

/// Record words keyed by two values instead of one (`set <node> <prop> <value>`).
const KEYED_BY_TWO: [&str; 1] = ["set"];
/// Record words that share another word's keys: `remove X` overrides `node X` (D8).
const KEY_ALIAS: [(&str, &str); 1] = [("remove", "node")];

/// Splits a contribution body into keyed records, validating it against the kind: legal
/// record words only, well-formed values and attribute lines, and for
/// [`Kind::Functions`] the exact form `function <name: word> <fn_id: u32> <arity: u8>`.
///
/// The body is lexed by `schemify-format` (format spec §2, the same reader as documents),
/// so a set the host accepts is exactly what the format accepts; [`text::chunks`] only cuts
/// the body into each record's own text for composition.
///
/// # Errors
///
/// `E_PARSE` on the first violation.
pub(crate) fn set_records(kind: Kind, body: &str) -> Result<Vec<SetRecord<'_>>, Code> {
    let chunks: Vec<&str> = text::chunks(body.as_bytes())
        .map(|c| std::str::from_utf8(c).expect("chunks of a str split at LF stay UTF-8"))
        .collect();
    let keys: Vec<String> = lex(body, |d| -> Result<Vec<String>, Code> {
        // An indented line before the first record attaches to the header: an orphan.
        let orphan = !d.attrs(0).is_empty() || d.verbatim(0).next().is_some();
        if orphan || d.record_count() as usize != chunks.len() + 1 {
            return Err(Code::Parse);
        }
        (1..d.record_count())
            .map(|r| record_key(kind, d, r))
            .collect()
    })??;
    let out: Vec<SetRecord<'_>> = keys
        .into_iter()
        .zip(chunks)
        .map(|(key, text)| SetRecord { key, text })
        .collect();
    debug_assert!(out.iter().all(|r| !r.key.is_empty()));
    Ok(out)
}

/// The composition key of record `r` of a lexed set, after checking its word against
/// `kind` and the exact `function` form.
fn record_key(kind: Kind, d: &FormatDoc<'_>, r: u32) -> Result<String, Code> {
    let word = d.record_word(r);
    if !kind.words().contains(&word) {
        return Err(Code::Parse);
    }
    let base = KEY_ALIAS
        .iter()
        .find(|(w, _)| *w == word)
        .map_or(word, |(_, alias)| alias);
    let first = d.field(r, 0).ok_or(Code::Parse)?;
    let mut key = format!("{base} {first}");
    if KEYED_BY_TWO.contains(&word) {
        key.push(' ');
        key.push_str(d.field(r, 1).ok_or(Code::Parse)?);
    }
    if kind == Kind::Functions {
        function_row(d, r)?;
    }
    assert!(key.len() > base.len());
    Ok(key)
}

/// `(name, fn_id, arity)` of a `function <name: word> <fn_id: u32> <arity: u8>` record.
///
/// # Errors
///
/// `E_PARSE` for any other shape.
pub(crate) fn function_row(d: &FormatDoc<'_>, r: u32) -> Result<(Box<str>, u32, u8), Code> {
    let name = d
        .field(r, 0)
        .filter(|n| text::is_word(n))
        .ok_or(Code::Parse)?;
    let fn_id = d
        .field(r, 1)
        .and_then(|f| f.parse().ok())
        .ok_or(Code::Parse)?;
    let arity = d
        .field(r, 2)
        .and_then(|a| a.parse().ok())
        .ok_or(Code::Parse)?;
    if d.field(r, 3).is_some() {
        return Err(Code::Parse);
    }
    Ok((name.into(), fn_id, arity))
}

/// What contribution sets and `spawn` argv lines are lexed as: a lockfile of a newer format
/// version, whose only known word (`locked`) no plugin record uses, so every record is kept
/// as an unknown record with its fields unescaped (the `doc` journal does the same).
const LEX_HEADER: &str = "schemify 65535 lock\n";

/// Lexes `body` with the format reader and hands the document to `read`.
///
/// # Errors
///
/// `E_PARSE` when the format reader rejects the text.
pub(crate) fn lex<T>(body: &str, read: impl FnOnce(&FormatDoc<'_>) -> T) -> Result<T, Code> {
    let mut src = String::with_capacity(LEX_HEADER.len() + body.len());
    src.push_str(LEX_HEADER);
    src.push_str(body);
    let d = schemify_format::parse(src.as_bytes()).map_err(|_| Code::Parse)?;
    assert!(
        d.version() > schemify_format::VERSION,
        "forward-compatible mode"
    );
    Ok(read(&d))
}

/// The values of one line (a `spawn` argv), lexed by the format reader.
///
/// # Errors
///
/// `E_PARSE` for a malformed value or a line break (the text must be exactly one line).
pub(crate) fn line_values(line: &str) -> Result<Vec<String>, Code> {
    if line.contains(['\n', '\r']) {
        return Err(Code::Parse);
    }
    let mut src = String::with_capacity(line.len() + 5);
    src.push_str("argv ");
    src.push_str(line);
    lex(&src, |d| match d.record_count() {
        2 => Ok(d.fields(1).map(str::to_owned).collect()),
        _ => Err(Code::Parse),
    })?
}

/// Decides whether `grants` allow an I/O request (§5). Pure; the import path and the fuzz
/// tests both call it.
///
/// # Errors
///
/// `E_DENIED` without the capability, for a path that is not project-relative (absolute,
/// `..`, `.` or empty segments, backslashes, drive or scheme colons), or for an
/// `argv[0]` that is not exactly a granted binary; `E_PARSE` for the wrong number of
/// arguments.
pub fn authorize(grants: &Grants, op: IoOp, args: &[String]) -> Result<(), Code> {
    let arity_ok = match op {
        IoOp::FsRead | IoOp::FsWrite | IoOp::Fetch => args.len() == 1,
        IoOp::Spawn => !args.is_empty(),
        IoOp::ClipboardGet | IoOp::ClipboardSet => args.is_empty(),
    };
    if !arity_ok {
        return Err(Code::Parse);
    }
    let allowed = grants.has(op.cap())
        && match op {
            IoOp::FsRead | IoOp::FsWrite => project_relative(&args[0]),
            IoOp::Spawn => grants.may_spawn(&args[0]),
            IoOp::Fetch => !args[0].is_empty(),
            IoOp::ClipboardGet | IoOp::ClipboardSet => true,
        };
    if allowed { Ok(()) } else { Err(Code::Denied) }
}

fn project_relative(path: &str) -> bool {
    path.len() <= 4096
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_records_key_and_validate() {
        let body =
            "node a item\n  parent menu\nremove b\nset c label \"x y\"\nstyle s\n  color red";
        let recs = set_records(Kind::Ui, body).unwrap();
        let keys: Vec<&str> = recs.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["node a", "node b", "set c label", "style s"]);
        assert_eq!(recs[0].text, "node a item\n  parent menu");
        assert_eq!(set_records(Kind::Ui, "command x y"), Err(Code::Parse));
        assert_eq!(set_records(Kind::Ui, "node"), Err(Code::Parse));
        assert_eq!(set_records(Kind::Ui, "  orphan x"), Err(Code::Parse));
        assert_eq!(
            set_records(Kind::Ui, "node a\n  attr \"open"),
            Err(Code::Parse)
        );
        assert!(set_records(Kind::Functions, "function gmid.width 0 3").is_ok());
        assert!(set_records(Kind::Functions, "function \"g.w\" 1 2\n  doc x").is_ok());
        for bad in [
            "function f",
            "function f x 3",
            "function f 0 300",
            "function f 0 3 extra",
            "function \"a b\" 0 1",
        ] {
            assert_eq!(set_records(Kind::Functions, bad), Err(Code::Parse), "{bad}");
        }
        assert_eq!(set_records(Kind::Commands, ""), Ok(Vec::new()));
        assert_eq!(
            set_records(Kind::Ui, "node a x\n\n# c\nnode b y\n")
                .unwrap()
                .len(),
            2
        );
        assert_eq!(set_records(Kind::Ui, "| x"), Err(Code::Parse));
    }

    #[test]
    fn spawn_argv_lexes_one_line() {
        assert_eq!(
            line_values(r#"ngspice -b "a b.cir""#),
            Ok(vec![
                "ngspice".to_owned(),
                "-b".to_owned(),
                "a b.cir".to_owned()
            ])
        );
        assert_eq!(line_values(""), Ok(Vec::new()));
        for bad in ["a\nb", "a\r", "\"open", "#x", "a\"b"] {
            assert_eq!(line_values(bad), Err(Code::Parse), "{bad:?}");
        }
    }

    proptest::proptest! {
        /// The guests' lexer (`abi::text`) and the format reader the host now uses agree on
        /// every line: the same values, or both reject it.
        #[test]
        fn abi_and_format_lexers_agree(line in "[ a-z0-9\"\\\\#|:.{}u\\-]{0,24}") {
            let abi: Result<Vec<String>, Code> = text::values(&line)
                .map(|v| v.map(std::borrow::Cow::into_owned))
                .collect();
            proptest::prop_assert_eq!(line_values(&line), abi);
        }
    }

    #[test]
    fn contributions_replace_and_clear() {
        let mut c = Contributions::new();
        let p = PluginId(1);
        c.set(p, Kind::Ui, "", "node a x").unwrap();
        let e = c.epoch();
        c.set(p, Kind::Ui, "", "node a x").unwrap();
        assert_eq!(c.epoch(), e, "identical set is not a change");
        c.set(p, Kind::Ui, "", "node b x").unwrap();
        assert_eq!(c.get(p, Kind::Ui, ""), Some("node b x"));
        c.set(p, Kind::Annotations, "erc", "mark a info b").unwrap();
        assert_eq!(c.len(), 2);
        c.set(p, Kind::Ui, "", "").unwrap();
        assert_eq!(c.get(p, Kind::Ui, ""), None);
        c.remove_plugin(p);
        assert!(c.is_empty());
    }

    #[test]
    fn authorize_follows_grants_and_paths() {
        let g = Grants::parse(&["read_project", "spawn:ngspice"]).unwrap();
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(authorize(&g, IoOp::FsRead, &a(&["lib/a.shcomp"])), Ok(()));
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "./a",
            "c:\\x",
            "a\\b",
            "",
        ] {
            assert_eq!(
                authorize(&g, IoOp::FsRead, &a(&[bad])),
                Err(Code::Denied),
                "{bad}"
            );
        }
        assert_eq!(authorize(&g, IoOp::FsWrite, &a(&["a"])), Err(Code::Denied));
        assert_eq!(authorize(&g, IoOp::Spawn, &a(&["ngspice", "-b"])), Ok(()));
        assert_eq!(
            authorize(&g, IoOp::Spawn, &a(&["/usr/bin/ngspice"])),
            Err(Code::Denied)
        );
        assert_eq!(authorize(&g, IoOp::Spawn, &a(&["sh"])), Err(Code::Denied));
        assert_eq!(authorize(&g, IoOp::Spawn, &[]), Err(Code::Parse));
        assert_eq!(
            authorize(&g, IoOp::Fetch, &a(&["https://x"])),
            Err(Code::Denied)
        );
        assert_eq!(authorize(&g, IoOp::ClipboardGet, &[]), Err(Code::Denied));
        assert_eq!(truncate("héllo", 2), "h");
    }
}
