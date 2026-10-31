//! egui's input events turned into the two outputs the host routes: the canvas's pointer rows
//! ([`InputEvent`], in logical px) and key chords looked up in the keymap.
//!
//! Contents: `pub fn pointer_events(ctx, rect, layer, salt, out)`; crate-only `canvas(v, ctx, rect, layer)`, `keys(v, ctx, typing)`, `chords_of(event, mods)`, `key_of(EKey) -> Option<Key>`

use egui::{Event, Key as EKey, LayerId, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect};

use super::View;
use crate::input::{Button, InputEvent};
use crate::keys::{Chord, Key, Mods};
use crate::state::UiState;
use crate::strings::Sym;

/// Pointer events over the canvas `rect` into the frame's canvas rows.
pub(crate) fn canvas(v: &mut View, ctx: &egui::Context, rect: Rect, layer: LayerId) {
    pointer_events(ctx, rect, layer, "canvas", &mut v.out.canvas);
}

/// The frame's pointer events over `rect` on `layer` (not covered by a float, popup or modal
/// dialog), and every pointer event while a press that started there is held, as rows in
/// logical px; a modifier change is a row before the event that shows it. `salt` keeps one
/// region's capture apart from another's.
pub fn pointer_events(
    ctx: &egui::Context,
    rect: Rect,
    layer: LayerId,
    salt: &str,
    out: &mut Vec<InputEvent>,
) {
    let capture_id = egui::Id::unique(("schemify.capture", salt));
    let mods_id = egui::Id::unique(("schemify.mods", salt));
    let mut captured = ctx.data(|d| d.get_temp::<bool>(capture_id).unwrap_or(false));
    let mut last_mods = ctx.data(|d| d.get_temp::<u8>(mods_id).unwrap_or(0));
    let modal = ctx.memory(|m| m.top_modal_layer().is_some());
    let over =
        |p: Pos2| rect.contains(p) && !modal && ctx.layer_id_at(p).is_none_or(|l| l == layer);
    let (events, hover) = ctx.input(|i| (i.events.clone(), i.pointer.latest_pos()));
    let mut mods_of = |m: &Modifiers, out: &mut Vec<InputEvent>| {
        let bits = mods(m).0;
        if bits != last_mods {
            last_mods = bits;
            out.push(InputEvent::Mods { mods: Mods(bits) });
        }
    };
    for e in &events {
        match e {
            Event::PointerMoved(p) if captured || over(*p) => {
                out.push(InputEvent::PointerMove { x: p.x, y: p.y });
            }
            Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } if captured || over(*pos) => {
                let Some(button) = button_of(*button) else {
                    continue;
                };
                mods_of(modifiers, out);
                let (x, y) = (pos.x, pos.y);
                out.push(if *pressed {
                    InputEvent::PointerDown { x, y, button }
                } else {
                    InputEvent::PointerUp { x, y, button }
                });
                captured = *pressed;
            }
            Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } => {
                let Some(p) = hover.filter(|p| over(*p)) else {
                    continue;
                };
                mods_of(modifiers, out);
                let lines = match unit {
                    MouseWheelUnit::Line => *delta,
                    MouseWheelUnit::Point => *delta / UiState::WHEEL_PX,
                    MouseWheelUnit::Page => *delta * 10.0,
                };
                out.push(InputEvent::Wheel {
                    x: p.x,
                    y: p.y,
                    dx: lines.x,
                    dy: lines.y,
                });
            }
            Event::PointerGone if !captured => out.push(InputEvent::PointerLeave),
            Event::Key { modifiers, .. } => mods_of(modifiers, out),
            _ => {}
        }
    }
    ctx.data_mut(|d| {
        d.insert_temp(capture_id, captured);
        d.insert_temp(mods_id, last_mods);
    });
}

/// Key chords of this frame run their keymap command, unless a text field has the keyboard or a
/// popup is open (no binding runs under an open menu).
pub(crate) fn keys(v: &mut View, ctx: &egui::Context, typing: bool) {
    // The modifiers each event was typed under: held from an earlier frame, or pressed and
    // released within this one.
    let held_id = egui::Id::unique("schemify.keys.held");
    let (events, last) = ctx.input(|i| (i.events.clone(), i.modifiers));
    let mut held = ctx
        .data(|d| d.get_temp::<Modifiers>(held_id))
        .unwrap_or(last);
    ctx.data_mut(|d| d.insert_temp(held_id, last));
    if v.st.keys_off || typing || ctx.egui_wants_keyboard_input() || egui::Popup::is_any_open(ctx) {
        return;
    }
    for e in &events {
        if let Event::ModifiersChanged(m) = e {
            held = *m;
        }
        for chord in chords_of(e, held) {
            if let Some(cmd) = v.t.keymap.lookup(chord) {
                v.out.commands.push(cmd, Sym::NONE, "");
            }
        }
    }
}

/// The chords one egui event presses: a named key, or any key with Ctrl, Alt or Command, from
/// its key event; printable characters from the text they type (so `%`, `*` and `Shift+R`
/// arrive as the layout produced them).
pub(crate) fn chords_of(e: &Event, held: Modifiers) -> Vec<Chord> {
    match e {
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            let named = key_of(*key).filter(|k| k.0 >= Key::NAMED || *key == EKey::Space);
            let combo = modifiers.ctrl || modifiers.alt || modifiers.mac_cmd || modifiers.command;
            // With Shift a digit or punctuation key types its shifted character.
            let shifted = |k: Key| match (modifiers.shift, char::from_u32(k.0)) {
                (true, Some(c)) => crate::keys::US_SHIFTED
                    .iter()
                    .find(|p| p.0 == c)
                    .map_or(k, |p| Key(p.1 as u32)),
                _ => k,
            };
            let k = if combo {
                key_of(*key).map(shifted)
            } else {
                named
            };
            k.map(|k| vec![Chord::new(mods(modifiers), k)])
                .unwrap_or_default()
        }
        Event::Text(t) if !(held.ctrl || held.alt || held.mac_cmd) => t
            .chars()
            .filter(|c| *c != ' ')
            .map(|c| Chord::new(mods(&held), Key(c as u32)))
            .collect(),
        _ => Vec::new(),
    }
}

/// Our key for an egui key: letters lowercase, digits and punctuation as their character,
/// named keys and F-keys as [`Key`] constants.
pub(crate) fn key_of(k: EKey) -> Option<Key> {
    let name = k.name();
    if let Some(n) = name.strip_prefix('F').and_then(|d| d.parse::<u8>().ok())
        && (1..=24).contains(&n)
    {
        return Some(Key::f(n));
    }
    let named = match k {
        EKey::Escape => Key::ESCAPE,
        EKey::Enter => Key::ENTER,
        EKey::Tab => Key::TAB,
        EKey::Backspace => Key::BACKSPACE,
        EKey::Delete => Key::DELETE,
        EKey::Insert => Key::INSERT,
        EKey::Home => Key::HOME,
        EKey::End => Key::END,
        EKey::PageUp => Key::PAGE_UP,
        EKey::PageDown => Key::PAGE_DOWN,
        EKey::ArrowLeft => Key::LEFT,
        EKey::ArrowRight => Key::RIGHT,
        EKey::ArrowUp => Key::UP,
        EKey::ArrowDown => Key::DOWN,
        EKey::Space => Key(' ' as u32),
        _ => {
            let sym = k.symbol_or_name();
            let mut cs = sym.chars();
            return match (cs.next(), cs.next()) {
                (Some(c), None) => Some(Key(c.to_ascii_lowercase() as u32)),
                _ => None,
            };
        }
    };
    Some(named)
}

// ---- private ----

fn mods(m: &Modifiers) -> Mods {
    let mut bits = 0;
    if m.ctrl {
        bits |= Mods::CTRL;
    }
    if m.shift {
        bits |= Mods::SHIFT;
    }
    if m.alt {
        bits |= Mods::ALT;
    }
    if m.mac_cmd {
        bits |= Mods::SUPER;
    }
    Mods(bits)
}

fn button_of(b: PointerButton) -> Option<Button> {
    match b {
        PointerButton::Primary => Some(Button::Left),
        PointerButton::Middle => Some(Button::Middle),
        PointerButton::Secondary => Some(Button::Right),
        PointerButton::Extra1 | PointerButton::Extra2 => None,
    }
}
