//! Events (§6): what the host tells a plugin, one per `schemify_on_event` call.
//!
//! Wire form: one line record `event <kind> <values…>` + LF. `io_done` and `rejected`
//! carry a binary payload after a single NUL byte (the record text never contains NUL,
//! because [`crate::text::push_value`] escapes control characters).

use crate::Code;
use crate::text::{self, Values};

/// One event. Borrowed from the buffer it was parsed from; identifier fields (`name`,
/// `dialect`, `bench`, `lang`) are bare values and never need unescaping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// The plugin was just loaded: declare contributions.
    Load,
    /// The plugin is about to be unloaded; contributions it makes now are dropped.
    Unload,
    /// A command the plugin contributed was invoked. `args` is raw line-record values text;
    /// iterate it with [`crate::text::values`].
    Command {
        /// Command name (`count_nets.run`).
        name: &'a str,
        /// Arguments as line-record values, possibly empty.
        args: &'a str,
    },
    /// A document was opened.
    DocOpened {
        /// Document handle (the `Docs.handle` column).
        doc: u32,
    },
    /// A document was closed; its handle is now stale.
    DocClosed {
        /// Document handle.
        doc: u32,
    },
    /// A document changed.
    DocChanged {
        /// Document handle.
        doc: u32,
        /// New epoch of the document's tables.
        epoch: u32,
    },
    /// The selection changed (read the `Selection` table).
    SelectionChanged,
    /// A netlist in a plugin-claimed dialect was requested.
    NetlistRequest {
        /// Dialect tag.
        dialect: &'a str,
        /// Bench as `lib:cell`.
        bench: &'a str,
    },
    /// A job in a plugin-claimed runner language should run.
    RunJob {
        /// Job id.
        job: u32,
        /// Runner language tag.
        lang: &'a str,
    },
    /// A job finished; its rows are in the `Results` table.
    RunDone {
        /// Job id.
        job: u32,
    },
    /// An inverted I/O or `eval` request completed (§5).
    IoDone {
        /// Handle the import returned.
        handle: u32,
        /// `>= 0` success (exit status for `spawn`), `< 0` a negated [`Code`].
        status: i32,
        /// Result bytes: file contents, response body, stdout, clipboard text, eval value.
        bytes: &'a [u8],
    },
    /// A timer the plugin requested fired.
    Timer {
        /// Timer id the plugin chose.
        id: u32,
    },
    /// A document was saved.
    Saved {
        /// Document handle.
        doc: u32,
    },
    /// An output record of this plugin was rejected (§8).
    Rejected {
        /// Why.
        code: Code,
        /// The rejected record, truncated by the host; not necessarily UTF-8.
        record: &'a [u8],
    },
}

impl<'a> Event<'a> {
    /// Returns the wire kind (`load`, `command`, …).
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Event::Load => "load",
            Event::Unload => "unload",
            Event::Command { .. } => "command",
            Event::DocOpened { .. } => "doc_opened",
            Event::DocClosed { .. } => "doc_closed",
            Event::DocChanged { .. } => "doc_changed",
            Event::SelectionChanged => "selection_changed",
            Event::NetlistRequest { .. } => "netlist_request",
            Event::RunJob { .. } => "run_job",
            Event::RunDone { .. } => "run_done",
            Event::IoDone { .. } => "io_done",
            Event::Timer { .. } => "timer",
            Event::Saved { .. } => "saved",
            Event::Rejected { .. } => "rejected",
        }
    }

    /// Appends the wire form to `out`.
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] (nothing appended) when an identifier field is not a legal bare value
    /// or `args` is not one line of well-formed values.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), Code> {
        let mut s = String::from("event ");
        s.push_str(self.kind_name());
        fn ident(s: &mut String, v: &str) -> Result<(), Code> {
            if !text::is_bare(v) {
                return Err(Code::Parse);
            }
            s.push(' ');
            s.push_str(v);
            Ok(())
        }
        fn num(s: &mut String, v: i64) {
            s.push(' ');
            s.push_str(&v.to_string());
        }
        let payload = match *self {
            Event::Load | Event::Unload | Event::SelectionChanged => None,
            Event::Command { name, args } => {
                ident(&mut s, name)?;
                if args.contains(['\n', '\r', '\0']) || text::values(args).any(|v| v.is_err()) {
                    return Err(Code::Parse);
                }
                let args = args.trim_matches([' ', '\t']);
                if !args.is_empty() {
                    s.push(' ');
                    s.push_str(args);
                }
                None
            }
            Event::DocOpened { doc } | Event::DocClosed { doc } | Event::Saved { doc } => {
                num(&mut s, doc.into());
                None
            }
            Event::DocChanged { doc, epoch } => {
                num(&mut s, doc.into());
                num(&mut s, epoch.into());
                None
            }
            Event::NetlistRequest { dialect, bench } => {
                ident(&mut s, dialect)?;
                ident(&mut s, bench)?;
                None
            }
            Event::RunJob { job, lang } => {
                num(&mut s, job.into());
                ident(&mut s, lang)?;
                None
            }
            Event::RunDone { job } => {
                num(&mut s, job.into());
                None
            }
            Event::Timer { id } => {
                num(&mut s, id.into());
                None
            }
            Event::IoDone {
                handle,
                status,
                bytes,
            } => {
                num(&mut s, handle.into());
                num(&mut s, status.into());
                Some(bytes)
            }
            Event::Rejected { code, record } => {
                s.push(' ');
                s.push_str(code.name());
                Some(record)
            }
        };
        s.push('\n');
        debug_assert!(!s.contains('\0'));
        out.extend_from_slice(s.as_bytes());
        if let Some(payload) = payload {
            out.push(0);
            out.extend_from_slice(payload);
        }
        Ok(())
    }

    /// Parses the wire form produced by [`Event::encode`].
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] for invalid UTF-8 before the payload, an unknown kind, wrong arity,
    /// a non-numeric number, an escaped identifier, or a payload on a kind without one.
    pub fn parse(buf: &'a [u8]) -> Result<Event<'a>, Code> {
        let (text, payload) = match buf.iter().position(|&b| b == 0) {
            Some(i) => (&buf[..i], Some(&buf[i + 1..])),
            None => (buf, None),
        };
        let text = std::str::from_utf8(text).map_err(|_| Code::Parse)?;
        let line = text.strip_suffix('\n').unwrap_or(text);
        if line.contains(['\n', '\r']) {
            return Err(Code::Parse);
        }
        let (word, rest) = text::split_word(line)?;
        let (kind, rest) = text::split_word(rest)?;
        if word != "event" {
            return Err(Code::Parse);
        }
        let mut v = text::values(rest);
        let event = match (kind, payload) {
            ("load", None) => Event::Load,
            ("unload", None) => Event::Unload,
            ("selection_changed", None) => Event::SelectionChanged,
            ("command", None) => {
                let name = v.need_plain()?;
                let args = v.rest();
                text::values(args).try_for_each(|a| a.map(drop))?;
                return Ok(Event::Command { name, args });
            }
            ("doc_opened", None) => Event::DocOpened { doc: v.need_num()? },
            ("doc_closed", None) => Event::DocClosed { doc: v.need_num()? },
            ("saved", None) => Event::Saved { doc: v.need_num()? },
            ("doc_changed", None) => Event::DocChanged {
                doc: v.need_num()?,
                epoch: v.need_num()?,
            },
            ("netlist_request", None) => Event::NetlistRequest {
                dialect: v.need_plain()?,
                bench: v.need_plain()?,
            },
            ("run_job", None) => Event::RunJob {
                job: v.need_num()?,
                lang: v.need_plain()?,
            },
            ("run_done", None) => Event::RunDone { job: v.need_num()? },
            ("timer", None) => Event::Timer { id: v.need_num()? },
            ("io_done", Some(bytes)) => Event::IoDone {
                handle: v.need_num()?,
                status: v.need_num()?,
                bytes,
            },
            ("rejected", Some(record)) => Event::Rejected {
                code: need_code(&mut v)?,
                record,
            },
            _ => return Err(Code::Parse),
        };
        v.end()?;
        Ok(event)
    }
}

fn need_code(v: &mut Values<'_>) -> Result<Code, Code> {
    Code::parse(v.need_plain()?).ok_or(Code::Parse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn round_trip(e: Event<'_>) {
        let mut buf = Vec::new();
        e.encode(&mut buf).unwrap();
        assert_eq!(
            Event::parse(&buf),
            Ok(e),
            "{}",
            String::from_utf8_lossy(&buf)
        );
    }

    #[test]
    fn every_kind_round_trips() {
        for e in [
            Event::Load,
            Event::Unload,
            Event::SelectionChanged,
            Event::Command {
                name: "count_nets.run",
                args: "",
            },
            Event::Command {
                name: "x",
                args: "1 \"two words\" lib:cell",
            },
            Event::DocOpened { doc: 3 },
            Event::DocClosed { doc: u32::MAX },
            Event::DocChanged { doc: 1, epoch: 9 },
            Event::NetlistRequest {
                dialect: "spectre",
                bench: "lib:tb",
            },
            Event::RunJob {
                job: 4,
                lang: "python",
            },
            Event::RunDone { job: 0 },
            Event::IoDone {
                handle: 7,
                status: -3,
                bytes: b"\0\xff raw\n",
            },
            Event::Timer { id: 2 },
            Event::Saved { doc: 5 },
            Event::Rejected {
                code: Code::Gone,
                record: b"edit 9 commit x",
            },
        ] {
            round_trip(e);
        }
    }

    #[test]
    fn malformed_events_are_parse_errors() {
        let mut ok = Vec::new();
        Event::Timer { id: 1 }.encode(&mut ok).unwrap();
        assert_eq!(ok, b"event timer 1\n");
        for bad in [
            &b"event"[..],
            b"event nope",
            b"evnt load",
            b"event load 1",
            b"event timer",
            b"event timer x",
            b"event timer -1",
            b"event load\0payload",
            b"event io_done 1 0",
            b"event rejected E_NOPE\0",
            b"event command \"esc\\n\"",
            b"event load\nevent load",
            b"\xffevent load",
        ] {
            assert_eq!(
                Event::parse(bad),
                Err(Code::Parse),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        let mut sink = Vec::new();
        assert_eq!(
            Event::Command {
                name: "a b",
                args: ""
            }
            .encode(&mut sink),
            Err(Code::Parse)
        );
        assert_eq!(
            Event::Command {
                name: "a",
                args: "\"open"
            }
            .encode(&mut sink),
            Err(Code::Parse)
        );
        assert_eq!(
            Event::Command {
                name: "a",
                args: "x\ny"
            }
            .encode(&mut sink),
            Err(Code::Parse)
        );
        assert!(sink.is_empty());
    }

    proptest! {
        #[test]
        fn io_done_round_trips(handle: u32, status: i32, bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            round_trip(Event::IoDone { handle, status, bytes: &bytes });
        }

        #[test]
        fn parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..128)) {
            let _ = Event::parse(&bytes);
        }

        #[test]
        fn command_args_survive(args in proptest::collection::vec("\\PC{0,8}", 0..4)) {
            let mut text = String::new();
            for a in &args {
                text.push(' ');
                text::push_value(&mut text, a);
            }
            let mut buf = Vec::new();
            Event::Command { name: "c", args: &text }.encode(&mut buf).unwrap();
            let Ok(Event::Command { args: got, .. }) = Event::parse(&buf) else { panic!() };
            let got: Vec<String> = text::values(got).map(|v| v.unwrap().into_owned()).collect();
            prop_assert_eq!(got, args);
        }
    }
}
