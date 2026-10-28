//! Events (§6): `Event` ↔ the bytes the host passes to `schemify_on_event`, one per call.
//!
//! Cites: plugin-abi §6 (event kinds and wire form), §5 (`io_done`), §8 (`rejected`). Wire
//! form: one line record `event <kind> <values…>` + LF. `io_done` and `rejected` carry a
//! binary payload after a single NUL byte (the record text never contains NUL, because
//! [`crate::text::push_value`] escapes control characters).
//!
//! Contents:
//! - `pub enum Event<'buf>`
//! - `Event::kind_name(&self) -> &'static str`
//! - `Event::encode(&self, out: &mut Vec<u8>) -> Result<(), Code>`
//! - `Event::parse(buf: &'buf [u8]) -> Result<Event<'buf>, Code>`

use crate::Code;
use crate::text;

/// One event. Borrowed from the buffer it was parsed from; identifier fields (`name`,
/// `dialect`, `bench`, `lang`) are bare values and never need unescaping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event<'buf> {
    /// The plugin was just loaded: declare contributions.
    Load,
    /// The plugin is about to be unloaded; contributions it makes now are dropped.
    Unload,
    /// A command the plugin contributed was invoked. `args` is raw line-record values text;
    /// iterate it with [`crate::text::values`].
    Command {
        /// Command name (`count_nets.run`).
        name: &'buf str,
        /// Arguments as line-record values, possibly empty.
        args: &'buf str,
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
        dialect: &'buf str,
        /// Bench as `lib:cell`.
        bench: &'buf str,
    },
    /// A job in a plugin-claimed runner language should run.
    RunJob {
        /// Job id.
        job: u32,
        /// Runner language tag.
        lang: &'buf str,
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
        bytes: &'buf [u8],
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
        record: &'buf [u8],
    },
}

impl<'buf> Event<'buf> {
    /// Returns the wire kind (`load`, `command`, …).
    #[must_use]
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
    /// [`Code::Parse`] when an identifier field is not a legal bare value or `args` is not one
    /// line of well-formed values; `out` is then left as it was.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), Code> {
        let start = out.len();
        match self.encode_line(out) {
            Ok(payload) => {
                debug_assert_eq!(out.last(), Some(&b'\n'));
                debug_assert!(
                    !out[start..].contains(&0),
                    "the record text never holds NUL"
                );
                if let Some(payload) = payload {
                    out.push(0);
                    out.extend_from_slice(payload);
                }
                // The pair of this encoder is `parse`: the bytes must read back as this kind.
                debug_assert_eq!(
                    Event::parse(&out[start..]).map(|event| event.kind_name()),
                    Ok(self.kind_name())
                );
                Ok(())
            }
            Err(code) => {
                debug_assert!(out.len() >= start, "encode_line only appends");
                out.truncate(start);
                Err(code)
            }
        }
    }

    /// Parses the wire form produced by [`Event::encode`].
    ///
    /// # Errors
    ///
    /// [`Code::Parse`] for invalid UTF-8 before the payload, an unknown kind, wrong arity,
    /// a non-numeric number, an escaped identifier, or a payload on a kind without one.
    pub fn parse(buf: &'buf [u8]) -> Result<Event<'buf>, Code> {
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
        let mut values = text::values(rest);
        let event = match (kind, payload) {
            ("load", None) => Event::Load,
            ("unload", None) => Event::Unload,
            ("selection_changed", None) => Event::SelectionChanged,
            ("command", None) => {
                let name = values.need_plain()?;
                let args = values.rest();
                for arg in text::values(args) {
                    arg?;
                }
                return Ok(Event::Command { name, args });
            }
            ("doc_opened", None) => Event::DocOpened {
                doc: values.need_num()?,
            },
            ("doc_closed", None) => Event::DocClosed {
                doc: values.need_num()?,
            },
            ("saved", None) => Event::Saved {
                doc: values.need_num()?,
            },
            ("doc_changed", None) => Event::DocChanged {
                doc: values.need_num()?,
                epoch: values.need_num()?,
            },
            ("netlist_request", None) => Event::NetlistRequest {
                dialect: values.need_plain()?,
                bench: values.need_plain()?,
            },
            ("run_job", None) => Event::RunJob {
                job: values.need_num()?,
                lang: values.need_plain()?,
            },
            ("run_done", None) => Event::RunDone {
                job: values.need_num()?,
            },
            ("timer", None) => Event::Timer {
                id: values.need_num()?,
            },
            ("io_done", Some(bytes)) => Event::IoDone {
                handle: values.need_num()?,
                status: values.need_num()?,
                bytes,
            },
            ("rejected", Some(record)) => Event::Rejected {
                code: Code::parse(values.need_plain()?).ok_or(Code::Parse)?,
                record,
            },
            _ => return Err(Code::Parse),
        };
        values.end()?;
        debug_assert_eq!(event.kind_name(), kind);
        debug_assert_eq!(payload.is_some(), event.has_payload());
        Ok(event)
    }
}

// ---- private ----

impl Event<'_> {
    /// Whether the wire form carries a binary payload after a NUL.
    const fn has_payload(&self) -> bool {
        matches!(self, Event::IoDone { .. } | Event::Rejected { .. })
    }

    /// Writes the record line (text and LF) and returns the payload still to append. On
    /// error `out` may hold a partial line; [`Event::encode`] truncates it.
    fn encode_line<'event>(&'event self, out: &mut Vec<u8>) -> Result<Option<&'event [u8]>, Code> {
        out.extend_from_slice(b"event ");
        out.extend_from_slice(self.kind_name().as_bytes());
        let payload = match *self {
            Event::Load | Event::Unload | Event::SelectionChanged => None,
            Event::Command { name, args } => {
                push_ident(out, name)?;
                if args.contains(['\n', '\r', '\0'])
                    || text::values(args).any(|value| value.is_err())
                {
                    return Err(Code::Parse);
                }
                let args = args.trim_matches([' ', '\t']);
                if !args.is_empty() {
                    out.push(b' ');
                    out.extend_from_slice(args.as_bytes());
                }
                None
            }
            Event::DocOpened { doc } | Event::DocClosed { doc } | Event::Saved { doc } => {
                push_num(out, i64::from(doc));
                None
            }
            Event::DocChanged { doc, epoch } => {
                push_num(out, i64::from(doc));
                push_num(out, i64::from(epoch));
                None
            }
            Event::NetlistRequest { dialect, bench } => {
                push_ident(out, dialect)?;
                push_ident(out, bench)?;
                None
            }
            Event::RunJob { job, lang } => {
                push_num(out, i64::from(job));
                push_ident(out, lang)?;
                None
            }
            Event::RunDone { job } | Event::Timer { id: job } => {
                push_num(out, i64::from(job));
                None
            }
            Event::IoDone {
                handle,
                status,
                bytes,
            } => {
                push_num(out, i64::from(handle));
                push_num(out, i64::from(status));
                Some(bytes)
            }
            Event::Rejected { code, record } => {
                out.push(b' ');
                out.extend_from_slice(code.name().as_bytes());
                Some(record)
            }
        };
        out.push(b'\n');
        debug_assert_eq!(payload.is_some(), self.has_payload());
        Ok(payload)
    }
}

/// Appends ` <ident>` for an identifier field, which must be a bare value.
fn push_ident(out: &mut Vec<u8>, ident: &str) -> Result<(), Code> {
    if !text::is_bare(ident) {
        return Err(Code::Parse);
    }
    out.push(b' ');
    out.extend_from_slice(ident.as_bytes());
    Ok(())
}

/// Appends ` <number>` in decimal.
fn push_num(out: &mut Vec<u8>, number: i64) {
    use std::io::Write as _;

    write!(out, " {number}").expect("writing to a Vec cannot fail");
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
        // A failed encode after an earlier event leaves the earlier one intact.
        let mut two = ok.clone();
        let bad = Event::NetlistRequest {
            dialect: "spectre",
            bench: "has space",
        };
        assert_eq!(bad.encode(&mut two), Err(Code::Parse));
        assert_eq!(two, ok);
    }

    /// Oracle: the spec §6 wire form, byte for byte, for one event of each payload shape.
    #[test]
    fn wire_form_is_the_spec_bytes() {
        let wire = |e: Event<'_>| {
            let mut buf = Vec::new();
            e.encode(&mut buf).unwrap();
            buf
        };
        assert_eq!(wire(Event::Load), b"event load\n");
        assert_eq!(
            wire(Event::DocChanged { doc: 1, epoch: 9 }),
            b"event doc_changed 1 9\n"
        );
        assert_eq!(
            wire(Event::Command {
                name: "x.run",
                args: "  1 \"a b\"  "
            }),
            b"event command x.run 1 \"a b\"\n"
        );
        assert_eq!(
            wire(Event::IoDone {
                handle: 2,
                status: -7,
                bytes: b"ab"
            }),
            b"event io_done 2 -7\n\0ab"
        );
        assert_eq!(
            wire(Event::Rejected {
                code: Code::Gone,
                record: b""
            }),
            b"event rejected E_GONE\n\0"
        );
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
