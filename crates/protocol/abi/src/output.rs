//! Output records (§7): what a plugin returns from `schemify_on_event`.
//!
//! Contributions are immediate-mode sets (each `contribute` replaces the plugin's previous
//! set of that kind and scope); everything else is a one-shot request validated by its
//! owner. [`Out`] writes them; [`Output::parse`] reads one record back.

use std::borrow::Cow;

use crate::text::{self, BodyLine};
use crate::{Code, EditMode, Kind, Level};

/// One parsed output record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output<'a> {
    /// `contribute <kind> [<scope>]` + verbatim body: replace this plugin's set.
    Contribute {
        /// Which set.
        kind: Kind,
        /// Annotation scope; empty for every other kind.
        scope: Cow<'a, str>,
        /// Line-record text of the set (verbatim lines joined with LF). Empty clears it.
        body: String,
    },
    /// `edit <doc> <mode> <label>` + verbatim op lines (edits spec §1).
    Edit {
        /// Document handle.
        doc: u32,
        /// Commit or stage.
        mode: EditMode,
        /// Undo-menu label.
        label: Cow<'a, str>,
        /// Op lines, at least one; validated by `doc`.
        ops: String,
    },
    /// `run <bench> <config> <corner>` + `vary <var> <value>` attributes.
    Run {
        /// Bench as `lib:cell`.
        bench: Cow<'a, str>,
        /// Named bench config.
        config: Cow<'a, str>,
        /// Corner name.
        corner: Cow<'a, str>,
        /// Variable overrides in record order.
        vary: Vec<(Cow<'a, str>, Cow<'a, str>)>,
    },
    /// `timer <id> <ms>`: deliver `Event::Timer { id }` after `ms` milliseconds.
    Timer {
        /// Plugin-chosen id.
        id: u32,
        /// Delay.
        ms: u32,
    },
    /// `log <level> <text>`.
    Log {
        /// Severity.
        level: Level,
        /// Message.
        text: Cow<'a, str>,
    },
}

impl<'a> Output<'a> {
    /// Parses one record chunk (as split by [`text::chunks`]).
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] for an unknown record word or contribution kind, wrong arity, a
    /// malformed value or body line, a body on a record that takes none, an attribute in a
    /// verbatim body, an `edit` without ops, or a `vary` with other than two values.
    pub fn parse(chunk: &'a str) -> Result<Output<'a>, Code> {
        let (head, body) = chunk.split_once('\n').unwrap_or((chunk, ""));
        let head = head.strip_suffix('\r').unwrap_or(head);
        if head.starts_with([' ', '\t']) {
            return Err(Code::Parse);
        }
        let (word, rest) = text::split_word(head)?;
        let mut v = text::values(rest);
        let out = match word {
            "contribute" => {
                let kind = Kind::parse(v.need_plain()?).ok_or(Code::Parse)?;
                let scope = if kind.scoped() {
                    v.need()?
                } else {
                    Cow::Borrowed("")
                };
                v.end()?;
                Output::Contribute {
                    kind,
                    scope,
                    body: text::verbatim_body(body)?,
                }
            }
            "edit" => {
                let doc = v.need_num()?;
                let mode = EditMode::parse(v.need_plain()?).ok_or(Code::Parse)?;
                let label = v.need()?;
                v.end()?;
                let ops = text::verbatim_body(body)?;
                if ops.trim().is_empty() {
                    return Err(Code::Parse);
                }
                Output::Edit {
                    doc,
                    mode,
                    label,
                    ops,
                }
            }
            "run" => {
                let (bench, config, corner) = (v.need()?, v.need()?, v.need()?);
                v.end()?;
                let mut vary = Vec::new();
                for line in text::lines(body) {
                    match text::body_line(line)? {
                        BodyLine::Blank | BodyLine::Comment => {}
                        BodyLine::Attr("vary", rest) => {
                            let mut a = text::values(rest);
                            vary.push((a.need()?, a.need()?));
                            a.end()?;
                        }
                        BodyLine::Attr(..) | BodyLine::Verbatim(_) => return Err(Code::Parse),
                    }
                }
                Output::Run {
                    bench,
                    config,
                    corner,
                    vary,
                }
            }
            "timer" => {
                let (id, ms) = (v.need_num()?, v.need_num()?);
                v.end()?;
                no_body(body)?;
                Output::Timer { id, ms }
            }
            "log" => {
                let level = Level::parse(v.need_plain()?).ok_or(Code::Parse)?;
                let text = v.need()?;
                v.end()?;
                no_body(body)?;
                Output::Log { level, text }
            }
            _ => return Err(Code::Parse),
        };
        Ok(out)
    }
}

fn no_body(body: &str) -> Result<(), Code> {
    for line in text::lines(body) {
        match text::body_line(line)? {
            BodyLine::Blank | BodyLine::Comment => {}
            BodyLine::Attr(..) | BodyLine::Verbatim(_) => return Err(Code::Parse),
        }
    }
    Ok(())
}

/// Output builder: appends well-formed records to one buffer.
///
/// Immediate mode: call `contribute*` with the *whole* current set every time it changes;
/// the host replaces the previous set and diffs. Values are quoted as needed, so any string
/// is safe to pass; bodies are line-record text written as verbatim lines.
///
/// # Examples
///
/// ```
/// use schemify_abi::{Out, Output, Kind, text};
///
/// let mut out = Out::new();
/// out.contribute_commands("command count_nets.run \"Count nets\"\n");
/// out.log("3 single-connection nets");
/// let records: Vec<Output> = text::chunks(out.as_str().as_bytes())
///     .map(|c| Output::parse(std::str::from_utf8(c).unwrap()))
///     .collect::<Result<_, _>>()?;
/// assert!(matches!(&records[0], Output::Contribute { kind: Kind::Commands, .. }));
/// # Ok::<(), schemify_abi::Code>(())
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Out {
    text: String,
}

impl Out {
    /// Returns an empty builder.
    pub fn new() -> Out {
        Out::default()
    }

    /// Returns the records written so far.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Returns the buffer, consuming the builder.
    pub fn into_string(self) -> String {
        self.text
    }

    /// Replaces this plugin's set of `kind` with `body` (line-record text).
    ///
    /// # Panics
    ///
    /// Panics if `kind` is scoped ([`Kind::Annotations`]); use [`Out::contribute_scoped`].
    pub fn contribute(&mut self, kind: Kind, body: &str) {
        assert!(!kind.scoped(), "{} needs a scope", kind.name());
        self.text.push_str("contribute ");
        self.text.push_str(kind.name());
        self.text.push('\n');
        text::push_verbatim(&mut self.text, body);
    }

    /// Replaces this plugin's set of a scoped kind for `scope`.
    ///
    /// # Panics
    ///
    /// Panics if `kind` is not scoped.
    pub fn contribute_scoped(&mut self, kind: Kind, scope: &str, body: &str) {
        assert!(kind.scoped(), "{} takes no scope", kind.name());
        self.text.push_str("contribute ");
        self.text.push_str(kind.name());
        self.text.push(' ');
        text::push_value(&mut self.text, scope);
        self.text.push('\n');
        text::push_verbatim(&mut self.text, body);
    }

    /// Shortcut for `contribute(Kind::Ui, body)`.
    pub fn contribute_ui(&mut self, body: &str) {
        self.contribute(Kind::Ui, body);
    }

    /// Shortcut for `contribute(Kind::Commands, body)`.
    pub fn contribute_commands(&mut self, body: &str) {
        self.contribute(Kind::Commands, body);
    }

    /// Requests an edit transaction on `doc`; `ops` are edits-spec §1 op lines.
    pub fn edit(&mut self, doc: u32, mode: EditMode, label: &str, ops: &str) {
        self.text.push_str("edit ");
        self.text.push_str(&doc.to_string());
        self.text.push(' ');
        self.text.push_str(mode.name());
        self.text.push(' ');
        text::push_value(&mut self.text, label);
        self.text.push('\n');
        text::push_verbatim(&mut self.text, ops);
    }

    /// Shortcut for `edit(doc, EditMode::Stage, label, ops)`: the user reviews it in the
    /// visual diff before it is committed.
    pub fn stage(&mut self, doc: u32, label: &str, ops: &str) {
        self.edit(doc, EditMode::Stage, label, ops);
    }

    /// Requests a simulation job.
    pub fn run(&mut self, bench: &str, config: &str, corner: &str, vary: &[(&str, &str)]) {
        self.text.push_str("run");
        for v in [bench, config, corner] {
            self.text.push(' ');
            text::push_value(&mut self.text, v);
        }
        self.text.push('\n');
        for (var, value) in vary {
            self.text.push_str("  vary ");
            text::push_value(&mut self.text, var);
            self.text.push(' ');
            text::push_value(&mut self.text, value);
            self.text.push('\n');
        }
    }

    /// Requests `Event::Timer { id }` after `ms` milliseconds.
    pub fn timer(&mut self, id: u32, ms: u32) {
        self.text.push_str(&format!("timer {id} {ms}\n"));
    }

    /// Logs `text` at [`Level::Info`].
    pub fn log(&mut self, text: &str) {
        self.log_at(Level::Info, text);
    }

    /// Logs `text` at `level`.
    pub fn log_at(&mut self, level: Level, text: &str) {
        self.text.push_str("log ");
        self.text.push_str(level.name());
        self.text.push(' ');
        text::push_value(&mut self.text, text);
        self.text.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn parse_all(text: &str) -> Vec<Result<Output<'_>, Code>> {
        text::chunks(text.as_bytes())
            .map(|c| Output::parse(std::str::from_utf8(c).unwrap()))
            .collect()
    }

    #[test]
    fn builder_output_parses_back() {
        let mut out = Out::new();
        out.contribute_ui("node tools.x item\n  parent menu.tools\n  label \"X\"\n");
        out.contribute_scoped(Kind::Annotations, "erc pass", "mark R1 warn \"hi\"\n");
        out.contribute(Kind::Functions, "");
        out.edit(4, EditMode::Commit, "move R1", "set_attr R1 at 10 20\n");
        out.stage(4, "size", "set_attr M1 w 2u");
        out.run(
            "lib:tb",
            "default",
            "tt",
            &[("vdd", "1.8"), ("w", "two words")],
        );
        out.timer(7, 250);
        out.log_at(Level::Warn, "line\nbreak \"q\"");
        let got = parse_all(out.as_str());
        let got: Vec<Output<'_>> = got.into_iter().map(Result::unwrap).collect();
        assert_eq!(got.len(), 8);
        assert_eq!(
            got[0],
            Output::Contribute {
                kind: Kind::Ui,
                scope: "".into(),
                body: "node tools.x item\n  parent menu.tools\n  label \"X\"".into()
            }
        );
        assert!(matches!(&got[1], Output::Contribute { scope, .. } if scope == "erc pass"));
        assert!(matches!(&got[2], Output::Contribute { body, .. } if body.is_empty()));
        assert!(matches!(
            &got[3],
            Output::Edit {
                doc: 4,
                mode: EditMode::Commit,
                ..
            }
        ));
        assert!(matches!(
            &got[4],
            Output::Edit {
                mode: EditMode::Stage,
                ..
            }
        ));
        assert!(matches!(&got[5], Output::Run { vary, .. } if vary[1].1 == "two words"));
        assert_eq!(got[6], Output::Timer { id: 7, ms: 250 });
        assert_eq!(
            got[7],
            Output::Log {
                level: Level::Warn,
                text: "line\nbreak \"q\"".into()
            }
        );
    }

    #[test]
    fn malformed_records_are_parse_errors() {
        for bad in [
            "frobnicate 1",
            "contribute",
            "contribute nonsense",
            "contribute annotations",
            "contribute ui extra",
            "contribute ui\n  attr x",
            "edit x commit l\n  | op",
            "edit 1 maybe l\n  | op",
            "edit 1 commit l",
            "edit 1 commit l extra\n  | op",
            "run a b",
            "run a b c\n  vary x",
            "run a b c\n  other x y",
            "timer 1",
            "timer 1 2\n  | body",
            "timer 1 99999999999",
            "log loud x",
            "log info",
            "log info \"open",
            "  orphan",
        ] {
            assert_eq!(Output::parse(bad), Err(Code::Parse), "{bad:?}");
        }
    }

    proptest! {
        #[test]
        fn logs_and_scopes_round_trip(msg in "\\PC{0,32}", scope in "\\PC{0,8}") {
            let mut out = Out::new();
            out.log(&msg);
            out.contribute_scoped(Kind::Annotations, &scope, "mark a info b");
            let got = parse_all(out.as_str());
            prop_assert_eq!(got.len(), 2);
            prop_assert_eq!(
                got[0].clone().unwrap(),
                Output::Log { level: Level::Info, text: msg.as_str().into() }
            );
            let ok = matches!(&got[1], Ok(Output::Contribute { scope: s, .. }) if *s == scope);
            prop_assert!(ok);
        }

        #[test]
        fn parse_never_panics(s in "\\PC{0,96}|[a-z \\n|\"\\\\]{0,96}") {
            for c in text::chunks(s.as_bytes()) {
                let _ = Output::parse(std::str::from_utf8(c).unwrap());
            }
        }
    }
}
