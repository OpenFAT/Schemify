//! The input tables of one frame: the windowing-system-independent event table in
//! ([`InputEvents`]), and what [`crate::route_input`] writes out ([`Routed`], whose
//! [`CommandRequests`] the app's dispatcher drains).
//!
//! Contents:
//! - `pub enum Button`, `pub enum InputEvent`
//! - `pub struct InputEvents`: `CAPACITY`, `push`, `push_text`, `push_preedit`, `clear`,
//!   `is_empty`
//! - `pub struct CommandRequests`: `arg`, `len`, `is_empty`; crate-only `push`
//! - `pub struct Routed`; crate-only `clear`

use std::ops::Range;

use crate::keys::{Chord, Mods};
use crate::strings::Sym;

/// Pointer button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    /// Primary.
    Left,
    /// Wheel button (pans the canvas in xschem).
    Middle,
    /// Secondary.
    Right,
}

/// One input event in logical pixels. Text payloads are byte ranges into [`InputEvents::text`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum InputEvent {
    /// Pointer moved.
    PointerMove {
        /// X.
        x: f32,
        /// Y.
        y: f32,
    },
    /// Button pressed.
    PointerDown {
        /// X.
        x: f32,
        /// Y.
        y: f32,
        /// Which button.
        button: Button,
    },
    /// Button released.
    PointerUp {
        /// X.
        x: f32,
        /// Y.
        y: f32,
        /// Which button.
        button: Button,
    },
    /// Pointer left the window.
    PointerLeave,
    /// Wheel or touchpad scroll, in lines (positive `dy` = away from the user).
    Wheel {
        /// X.
        x: f32,
        /// Y.
        y: f32,
        /// Horizontal lines.
        dx: f32,
        /// Vertical lines.
        dy: f32,
    },
    /// Key pressed (auto-repeat included), normalized as a [`Chord`].
    Key {
        /// The chord.
        chord: Chord,
    },
    /// Committed text (typing or IME commit).
    Text {
        /// Start byte in [`InputEvents::text`].
        start: u32,
        /// End byte.
        end: u32,
    },
    /// IME preedit (empty range clears it).
    Preedit {
        /// Start byte in [`InputEvents::text`].
        start: u32,
        /// End byte.
        end: u32,
    },
    /// Assistive-technology "click" on a node (AccessKit `Action::Click`).
    Activate {
        /// Target node id.
        node: Sym,
    },
    /// The held modifiers changed (pointer gestures such as Shift+wheel read them; key chords
    /// carry their own).
    Mods {
        /// Modifier bits now held.
        mods: Mods,
    },
}

/// The input event table for one frame.
#[derive(Clone, Debug, Default)]
pub struct InputEvents {
    /// Events in arrival order.
    pub events: Vec<InputEvent>,
    /// Text payloads.
    pub text: String,
}

/// `command_requests`: commands to run, consumed by the app's dispatcher.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandRequests {
    /// Command names.
    pub command: Vec<Sym>,
    /// Node that issued it (`Sym::NONE` for key bindings).
    pub node: Vec<Sym>,
    /// End byte of each argument in `args` (the text of a submitted field; usually empty).
    pub arg_end: Vec<u32>,
    /// Argument bytes.
    pub args: String,
}

/// Everything [`route_input`](crate::route_input) produces for one frame.
#[derive(Clone, Debug, Default)]
pub struct Routed {
    /// Commands to dispatch.
    pub commands: CommandRequests,
    /// Events that belong to the canvas (pointer over it or captured by a drag started on it).
    pub canvas: Vec<InputEvent>,
    /// The layout is stale (menus opened/closed, focus changed size).
    pub relayout: bool,
    /// The picture changed (hover, editing).
    pub redraw: bool,
}

impl InputEvents {
    /// Events kept per frame; extra events are dropped (a stalled frame must not grow memory).
    pub const CAPACITY: usize = 4096;

    /// Appends an event; returns false when the table is full and the event was dropped.
    pub fn push(&mut self, e: InputEvent) -> bool {
        if self.events.len() >= Self::CAPACITY {
            debug_assert_eq!(self.events.len(), Self::CAPACITY, "never past capacity");
            return false;
        }
        let before = self.events.len();
        self.events.push(e);
        debug_assert_eq!(self.events.len(), before + 1);
        true
    }

    /// Appends a committed-text event; returns false when the table is full.
    ///
    /// # Panics
    ///
    /// Panics when one frame's text payloads exceed 4 GiB.
    pub fn push_text(&mut self, s: &str) -> bool {
        let range = self.payload(s);
        debug_assert_eq!(
            range.end as usize,
            self.text.len(),
            "the payload is the newest text"
        );
        let pushed = self.push(InputEvent::Text {
            start: range.start,
            end: range.end,
        });
        debug_assert!(self.events.len() <= Self::CAPACITY);
        pushed
    }

    /// Appends an IME preedit event; returns false when the table is full.
    ///
    /// # Panics
    ///
    /// Panics when one frame's text payloads exceed 4 GiB.
    pub fn push_preedit(&mut self, s: &str) -> bool {
        let range = self.payload(s);
        debug_assert_eq!(
            range.end as usize,
            self.text.len(),
            "the payload is the newest text"
        );
        let pushed = self.push(InputEvent::Preedit {
            start: range.start,
            end: range.end,
        });
        debug_assert!(self.events.len() <= Self::CAPACITY);
        pushed
    }

    /// Empties the table, keeping its allocation.
    pub fn clear(&mut self) {
        self.events.clear();
        self.text.clear();
        debug_assert!(self.is_empty());
    }

    /// True when no event is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        debug_assert!(
            self.events.len() <= Self::CAPACITY,
            "the table stays bounded"
        );
        self.events.is_empty()
    }
}

impl CommandRequests {
    /// Appends one request; the three columns and `arg_end` stay in step.
    pub(crate) fn push(&mut self, command: Sym, node: Sym, arg: &str) {
        let before = self.args.len();
        self.command.push(command);
        self.node.push(node);
        self.args.push_str(arg);
        let end = u32::try_from(self.args.len()).expect("one frame's arguments stay under 4 GiB");
        self.arg_end.push(end);
        debug_assert_eq!(self.node.len(), self.command.len());
        debug_assert_eq!(self.arg_end.len(), self.command.len());
        debug_assert_eq!(end as usize - before, arg.len());
    }

    /// Argument of request `i`.
    ///
    /// # Panics
    ///
    /// Panics if `i` is not below [`CommandRequests::len`].
    #[must_use]
    pub fn arg(&self, i: usize) -> &str {
        assert!(i < self.len(), "no request {i}");
        debug_assert_eq!(self.arg_end.len(), self.command.len());
        let start = if i == 0 {
            0
        } else {
            self.arg_end[i - 1] as usize
        };
        let end = self.arg_end[i] as usize;
        debug_assert!(start <= end, "argument ends ascend");
        &self.args[start..end]
    }

    /// Number of requests.
    #[must_use]
    pub fn len(&self) -> usize {
        debug_assert_eq!(self.node.len(), self.command.len());
        self.command.len()
    }

    /// True when nothing was requested.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        debug_assert_eq!(self.arg_end.len(), self.command.len(), "columns in step");
        self.command.is_empty()
    }
}

impl Routed {
    /// Resets the output for a new frame, keeping the canvas column's allocation.
    pub(crate) fn clear(&mut self) {
        self.commands = CommandRequests::default();
        self.canvas.clear();
        self.relayout = false;
        self.redraw = false;
        debug_assert!(self.canvas.is_empty());
    }
}

// ---- private ----

impl InputEvents {
    /// Appends `s` to the text payloads; returns its byte range.
    fn payload(&mut self, s: &str) -> Range<u32> {
        let start = u32::try_from(self.text.len()).expect("one frame's text stays under 4 GiB");
        self.text.push_str(s);
        let end = u32::try_from(self.text.len()).expect("one frame's text stays under 4 GiB");
        debug_assert_eq!((end - start) as usize, s.len());
        start..end
    }
}
