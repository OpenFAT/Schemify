//! Input: winit's window events in, the `ui` crate's [`InputEvents`] out (pointer, wheel,
//! keys with their modifiers, typed text and IME composition), in logical px.
//!
//! Contents:
//! - `Shell::input` (`pub(super)`)

use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

use schemify_ui::{Button, Chord, InputEvent, Key, Mods};

use super::{App, Shell};

impl<A: App> Shell<A> {
    /// Queues the input `event` carries (nothing for any other event).
    pub(super) fn input(&mut self, event: WindowEvent) {
        let s = self.scale;
        let at = self.pointer;
        match event {
            WindowEvent::ModifiersChanged(m) => {
                self.mods = mods_of(m.state());
                self.events.push(InputEvent::Mods {
                    mods: Mods(self.mods),
                });
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = ((position.x / s) as f32, (position.y / s) as f32);
                self.events.push(InputEvent::PointerMove {
                    x: self.pointer.0,
                    y: self.pointer.1,
                });
            }
            WindowEvent::CursorLeft { .. } => {
                self.events.push(InputEvent::PointerLeave);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    MouseButton::Left => Button::Left,
                    MouseButton::Middle => Button::Middle,
                    MouseButton::Right => Button::Right,
                    _ => return,
                };
                let (x, y) = at;
                self.events.push(match state {
                    ElementState::Pressed => InputEvent::PointerDown { x, y, button },
                    ElementState::Released => InputEvent::PointerUp { x, y, button },
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x, y),
                    // ~20 px per wheel line on every platform we target.
                    MouseScrollDelta::PixelDelta(p) => ((p.x / 20.0) as f32, (p.y / 20.0) as f32),
                };
                self.events.push(InputEvent::Wheel {
                    x: at.0,
                    y: at.1,
                    dx,
                    dy,
                });
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state == ElementState::Pressed => {
                self.push_key(&event.logical_key, event.text.as_deref());
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                self.events.push_preedit(&text);
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.events.push_preedit("");
                self.events.push_text(&text);
            }
            _ => {}
        }
    }
}

// ---- private ----

impl<A: App> Shell<A> {
    fn push_key(&mut self, key: &WKey, text: Option<&str>) {
        let k = match key {
            WKey::Character(s) => s.chars().next().map(|c| Key(c as u32)),
            WKey::Named(n) => named_key(*n),
            _ => None,
        };
        if let Some(k) = k {
            self.events.push(InputEvent::Key {
                chord: Chord::new(Mods(self.mods), k),
            });
        }
        if let Some(t) = text.filter(|t| t.chars().all(|c| !c.is_control())) {
            self.events.push_text(t);
        }
    }
}

fn named_key(n: NamedKey) -> Option<Key> {
    Some(match n {
        NamedKey::Escape => Key::ESCAPE,
        NamedKey::Enter => Key::ENTER,
        NamedKey::Tab => Key::TAB,
        NamedKey::Backspace => Key::BACKSPACE,
        NamedKey::Delete => Key::DELETE,
        NamedKey::Insert => Key::INSERT,
        NamedKey::Home => Key::HOME,
        NamedKey::End => Key::END,
        NamedKey::PageUp => Key::PAGE_UP,
        NamedKey::PageDown => Key::PAGE_DOWN,
        NamedKey::ArrowLeft => Key::LEFT,
        NamedKey::ArrowRight => Key::RIGHT,
        NamedKey::ArrowUp => Key::UP,
        NamedKey::ArrowDown => Key::DOWN,
        NamedKey::PrintScreen => Key::PRINT,
        NamedKey::Space => Key(' ' as u32),
        NamedKey::F1 => Key::f(1),
        NamedKey::F2 => Key::f(2),
        NamedKey::F3 => Key::f(3),
        NamedKey::F4 => Key::f(4),
        NamedKey::F5 => Key::f(5),
        NamedKey::F6 => Key::f(6),
        NamedKey::F7 => Key::f(7),
        NamedKey::F8 => Key::f(8),
        NamedKey::F9 => Key::f(9),
        NamedKey::F10 => Key::f(10),
        NamedKey::F11 => Key::f(11),
        NamedKey::F12 => Key::f(12),
        _ => return None,
    })
}

fn mods_of(m: ModifiersState) -> u8 {
    let mut out = 0;
    if m.control_key() {
        out |= Mods::CTRL;
    }
    if m.shift_key() {
        out |= Mods::SHIFT;
    }
    if m.alt_key() {
        out |= Mods::ALT;
    }
    if m.super_key() {
        out |= Mods::SUPER;
    }
    out
}
