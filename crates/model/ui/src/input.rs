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
            return false;
        }
        self.events.push(e);
        true
    }

    /// Appends a committed-text event.
    pub fn push_text(&mut self, s: &str) -> bool {
        let (start, end) = self.payload(s);
        self.push(InputEvent::Text { start, end })
    }

    /// Appends an IME preedit event.
    pub fn push_preedit(&mut self, s: &str) -> bool {
        let (start, end) = self.payload(s);
        self.push(InputEvent::Preedit { start, end })
    }

    /// Appends `s` to the text payloads; returns its byte range.
    fn payload(&mut self, s: &str) -> (u32, u32) {
        let start = self.text.len() as u32;
        self.text.push_str(s);
        (start, self.text.len() as u32)
    }

    /// Empties the table, keeping its allocation.
    pub fn clear(&mut self) {
        self.events.clear();
        self.text.clear();
    }

    /// True when no event is queued.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl CommandRequests {
    pub(crate) fn push(&mut self, command: Sym, node: Sym, arg: &str) {
        self.command.push(command);
        self.node.push(node);
        self.args.push_str(arg);
        self.arg_end.push(self.args.len() as u32);
    }

    /// Argument of request `i`.
    pub fn arg(&self, i: usize) -> &str {
        let start = if i == 0 {
            0
        } else {
            self.arg_end[i - 1] as usize
        };
        &self.args[start..self.arg_end[i] as usize]
    }

    /// Number of requests.
    pub fn len(&self) -> usize {
        self.command.len()
    }

    /// True when nothing was requested.
    pub fn is_empty(&self) -> bool {
        self.command.is_empty()
    }
}

impl Routed {
    pub(crate) fn clear(&mut self) {
        self.commands = CommandRequests::default();
        self.canvas.clear();
        self.relayout = false;
        self.redraw = false;
    }
}
