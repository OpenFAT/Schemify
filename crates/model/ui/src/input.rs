//! The input tables of one frame: the scripted event table ([`InputEvents`]: tests, the RPC
//! stream and the canvas's pointer rows), its conversion to egui's [`egui::RawInput`]
//! ([`InputEvents::raw`]), and what [`crate::show`] writes out ([`Routed`], whose
//! [`CommandRequests`] the editor's dispatcher drains).
//!
//! Contents:
//! - `pub enum Button`, `pub enum InputEvent`
//! - `pub struct InputEvents`: `CAPACITY`, `push`, `push_text`, `push_preedit`, `clear`,
//!   `is_empty`, `raw`
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

/// Everything [`crate::show`] produces for one frame.
#[derive(Clone, Debug, Default)]
pub struct Routed {
    /// Commands to dispatch.
    pub commands: CommandRequests,
    /// Events that belong to the canvas (pointer over it or captured by a drag started on it).
    pub canvas: Vec<InputEvent>,
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
        debug_assert!(self.text.is_empty(), "payloads go with their events");
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
    pub fn clear(&mut self) {
        self.commands = CommandRequests::default();
        self.canvas.clear();
        debug_assert!(self.canvas.is_empty());
        debug_assert!(self.commands.is_empty());
    }
}

impl InputEvents {
    /// The table as egui input for a `viewport` window at 1 device px per logical px: what a
    /// windowing backend would have sent for the same events. A key chord becomes the text it
    /// types when it is printable without Ctrl or Alt, otherwise a key event; `Mods` rows set
    /// the modifiers of the events after them.
    ///
    /// # Panics
    ///
    /// Panics if a text row's range lies outside [`InputEvents::text`] or the viewport is
    /// negative.
    #[must_use]
    pub fn raw(&self, viewport: (f32, f32)) -> egui::RawInput {
        use egui::{Event, Modifiers, PointerButton, Pos2};
        assert!(
            viewport.0 >= 0.0 && viewport.1 >= 0.0,
            "a viewport of non-negative size"
        );
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(viewport.0, viewport.1),
            )),
            ..egui::RawInput::default()
        };
        let mut m = Modifiers::NONE;
        let button = |b: Button| match b {
            Button::Left => PointerButton::Primary,
            Button::Middle => PointerButton::Middle,
            Button::Right => PointerButton::Secondary,
        };
        for e in &self.events {
            let ev = match *e {
                InputEvent::PointerMove { x, y } => Event::PointerMoved(Pos2::new(x, y)),
                InputEvent::PointerDown { x, y, button: b }
                | InputEvent::PointerUp { x, y, button: b } => Event::PointerButton {
                    pos: Pos2::new(x, y),
                    button: button(b),
                    pressed: matches!(e, InputEvent::PointerDown { .. }),
                    modifiers: m,
                },
                InputEvent::PointerLeave => Event::PointerGone,
                InputEvent::Wheel { x, y, dx, dy } => {
                    // A backend reports where the pointer is before it scrolls there.
                    raw.events.push(Event::PointerMoved(Pos2::new(x, y)));
                    Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::vec2(dx, dy),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                        modifiers: m,
                    }
                }
                InputEvent::Text { start, end } => {
                    Event::Text(self.text[start as usize..end as usize].to_owned())
                }
                InputEvent::Preedit { start, end } => Event::Ime(egui::ImeEvent::Preedit {
                    text: self.text[start as usize..end as usize].to_owned(),
                    active_range_chars: None,
                }),
                InputEvent::Mods { mods } => {
                    m = Modifiers {
                        alt: mods.0 & Mods::ALT != 0,
                        ctrl: mods.0 & Mods::CTRL != 0,
                        shift: mods.0 & Mods::SHIFT != 0,
                        mac_cmd: mods.0 & Mods::SUPER != 0,
                        command: mods.0 & Mods::CTRL != 0,
                    };
                    raw.events.push(Event::ModifiersChanged(m));
                    continue;
                }
                InputEvent::Key { chord } => {
                    raw.events.extend(chord_events(chord));
                    continue;
                }
            };
            raw.events.push(ev);
        }
        debug_assert!(
            raw.events.len() <= 2 * self.events.len(),
            "at most two egui events per row"
        );
        raw
    }
}

// ---- private ----

/// The egui events a backend sends for one key chord.
fn chord_events(chord: Chord) -> Vec<egui::Event> {
    use crate::keys::Key;
    let bits = chord.mods().0;
    let modifiers = egui::Modifiers {
        alt: bits & Mods::ALT != 0,
        ctrl: bits & Mods::CTRL != 0,
        shift: bits & Mods::SHIFT != 0,
        mac_cmd: bits & Mods::SUPER != 0,
        command: bits & Mods::CTRL != 0,
    };
    let k = chord.key().0;
    let printable = k < Key::NAMED && k != u32::from(' ');
    if printable && bits & (Mods::CTRL | Mods::ALT | Mods::SUPER) == 0 {
        let c = char::from_u32(k).unwrap_or('?');
        let c = if bits & Mods::SHIFT != 0 {
            c.to_ascii_uppercase()
        } else {
            c
        };
        return vec![egui::Event::Text(c.to_string())];
    }
    let name = match chord.key() {
        Key::ESCAPE => "Escape".to_owned(),
        Key::ENTER => "Enter".to_owned(),
        Key::TAB => "Tab".to_owned(),
        Key::BACKSPACE => "Backspace".to_owned(),
        Key::DELETE => "Delete".to_owned(),
        Key::INSERT => "Insert".to_owned(),
        Key::HOME => "Home".to_owned(),
        Key::END => "End".to_owned(),
        Key::PAGE_UP => "PageUp".to_owned(),
        Key::PAGE_DOWN => "PageDown".to_owned(),
        Key::LEFT => "ArrowLeft".to_owned(),
        Key::RIGHT => "ArrowRight".to_owned(),
        Key::UP => "ArrowUp".to_owned(),
        Key::DOWN => "ArrowDown".to_owned(),
        _ if k > Key::NAMED + 0x100 => format!("F{}", k - Key::NAMED - 0x100),
        _ => {
            // A shifted symbol is its US-layout key with Shift, as a backend reports it.
            let c = char::from_u32(k).unwrap_or('?');
            let base = crate::keys::US_SHIFTED
                .iter()
                .find(|p| p.1 == c)
                .map_or(c, |p| p.0);
            if base != c {
                return vec![egui::Event::Key {
                    key: egui::Key::from_name(&base.to_string()).unwrap_or(egui::Key::Num0),
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        shift: true,
                        ..modifiers
                    },
                }];
            }
            c.to_ascii_uppercase().to_string()
        }
    };
    let Some(key) = egui::Key::from_name(&name) else {
        return Vec::new();
    };
    vec![egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }]
}

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
