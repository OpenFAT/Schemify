//! Keys and chords: a key event or a `key` record in, one normalized packed [`Chord`] out,
//! so the keymap is a sorted `u32` column searched once per key press.
//!
//! Contents:
//! - `pub struct Mods(pub u8)`: `CTRL`, `SHIFT`, `ALT`, `SUPER`
//! - `pub struct Key(pub u32)`: `NAMED`, the named keys `ESCAPE`, `ENTER`, `TAB`, `BACKSPACE`,
//!   `DELETE`, `INSERT`, `HOME`, `END`, `PAGE_UP`, `PAGE_DOWN`, `LEFT`, `RIGHT`, `UP`, `DOWN`,
//!   `PRINT`; `f(u8) -> Key`, `parse(&str) -> Option<Key>`
//! - `pub struct Chord`: `new(Mods, Key) -> Chord`, `parse(&str) -> Option<Chord>`,
//!   `mods(self) -> Mods`, `key(self) -> Key`

/// Keyboard modifier bits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Mods(pub u8);

/// A key: a Unicode scalar for printable keys, or a named key at [`Key::NAMED`]` + n`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key(pub u32);

/// A key plus modifiers, normalized so that the same physical chord always packs the same way.
///
/// Normalization (shared by [`Chord::parse`] and event translation): letters are lowercased and
/// keep `Shift`; other printable characters drop `Shift`, because the character already encodes
/// it (`%` is Shift+5 on a US layout, and xschem writes it as plain `%`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Chord(u32);

/// Offset of the function keys from [`Key::NAMED`], past every named key.
const F_KEYS: u32 = 0x100;

const _: () = assert!(
    NAMED_KEYS.len() < F_KEYS as usize,
    "named keys end before F1"
);
const _: () = assert!(
    Key::NAMED + F_KEYS + 24 < 1 << 24,
    "every key fits a chord's 24 bits"
);

/// Record spellings of the named keys, in code order from [`Key::NAMED`].
const NAMED_KEYS: [&str; 15] = [
    "Esc",
    "Enter",
    "Tab",
    "BackSpace",
    "Del",
    "Ins",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Left",
    "Right",
    "Up",
    "Down",
    "Print",
];

impl Mods {
    /// Control.
    pub const CTRL: u8 = 1;
    /// Shift.
    pub const SHIFT: u8 = 2;
    /// Alt / Option.
    pub const ALT: u8 = 4;
    /// Super / Command / Windows.
    pub const SUPER: u8 = 8;
}

impl Key {
    /// First named-key code (above the Unicode range).
    pub const NAMED: u32 = 0x11_0000;
    /// Escape.
    pub const ESCAPE: Key = Key(Self::NAMED);
    /// Enter / Return.
    pub const ENTER: Key = Key(Self::NAMED + 1);
    /// Tab.
    pub const TAB: Key = Key(Self::NAMED + 2);
    /// Backspace.
    pub const BACKSPACE: Key = Key(Self::NAMED + 3);
    /// Delete.
    pub const DELETE: Key = Key(Self::NAMED + 4);
    /// Insert.
    pub const INSERT: Key = Key(Self::NAMED + 5);
    /// Home.
    pub const HOME: Key = Key(Self::NAMED + 6);
    /// End.
    pub const END: Key = Key(Self::NAMED + 7);
    /// Page up.
    pub const PAGE_UP: Key = Key(Self::NAMED + 8);
    /// Page down.
    pub const PAGE_DOWN: Key = Key(Self::NAMED + 9);
    /// Arrow left.
    pub const LEFT: Key = Key(Self::NAMED + 10);
    /// Arrow right.
    pub const RIGHT: Key = Key(Self::NAMED + 11);
    /// Arrow up.
    pub const UP: Key = Key(Self::NAMED + 12);
    /// Arrow down.
    pub const DOWN: Key = Key(Self::NAMED + 13);
    /// Print screen.
    pub const PRINT: Key = Key(Self::NAMED + 14);

    /// Function key `F<n>` for `n` in `1..=24`.
    ///
    /// # Panics
    ///
    /// Panics if `n` is outside `1..=24`.
    #[must_use]
    pub fn f(n: u8) -> Key {
        assert!((1..=24).contains(&n), "F-key out of range");
        let key = Key(Self::NAMED + F_KEYS + u32::from(n));
        debug_assert!(
            key.0 > Self::NAMED + NAMED_KEYS.len() as u32,
            "F-keys follow the named keys"
        );
        key
    }

    /// Parses a key name: one character, a named key (case-insensitive, with the aliases
    /// `Escape`, `Return`, `Delete`, `Insert`, `Space`), or `F1`..`F24`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Key> {
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Some(Key(c as u32));
        }
        let alias = match s.to_ascii_lowercase().as_str() {
            "escape" => "Esc",
            "return" => "Enter",
            "delete" => "Del",
            "insert" => "Ins",
            "space" => return Some(Key(' ' as u32)),
            _ => s,
        };
        if let Some(i) = NAMED_KEYS
            .iter()
            .position(|n| n.eq_ignore_ascii_case(alias))
        {
            return Some(Key(Self::NAMED + i as u32));
        }
        let n: u8 = s.strip_prefix(['F', 'f'])?.parse().ok()?;
        let key = (1..=24).contains(&n).then(|| Key::f(n));
        debug_assert!(
            key.is_none_or(|k| k.0 < 1 << 24),
            "a key fits a chord's 24 bits"
        );
        key
    }
}

impl Chord {
    /// Builds a normalized chord.
    ///
    /// # Panics
    ///
    /// Panics if `key`'s code is wider than 24 bits (no [`Key`] constant or parse is).
    #[must_use]
    pub fn new(mods: Mods, key: Key) -> Chord {
        assert!(key.0 < 1 << 24, "key code wider than 24 bits");
        let mut m = mods.0;
        let mut k = key.0;
        if k < Key::NAMED
            && let Some(c) = char::from_u32(k)
        {
            if c.is_alphabetic() {
                let mut lower = c.to_lowercase();
                if let (Some(l), None) = (lower.next(), lower.next()) {
                    k = l as u32;
                }
            } else if c != ' ' {
                m &= !Mods::SHIFT;
            }
        }
        let chord = Chord((u32::from(m) << 24) | k);
        // Normalization only drops Shift and lowercases: every other bit survives.
        debug_assert_eq!(chord.mods().0 & !Mods::SHIFT, mods.0 & !Mods::SHIFT);
        debug_assert_eq!(chord.key().0 >= Key::NAMED, key.0 >= Key::NAMED);
        chord
    }

    /// Parses `Ctrl+Shift+S`, `Shift-Ins`, `Alt+-`, `%`: modifiers (`Ctrl`/`Control`, `Shift`,
    /// `Alt`, `Super`/`Cmd`, case-insensitive) each followed by `+` or `-`, then a [`Key`] name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Chord> {
        const MODS: [(&str, u8); 6] = [
            ("ctrl", Mods::CTRL),
            ("control", Mods::CTRL),
            ("shift", Mods::SHIFT),
            ("alt", Mods::ALT),
            ("super", Mods::SUPER),
            ("cmd", Mods::SUPER),
        ];
        let mut mods = 0u8;
        let mut rest = s;
        // Bounded: each pass consumes one modifier and there are at most 4 distinct bits.
        for _ in 0..8 {
            let hit = MODS.iter().find(|(name, _)| {
                rest.len() > name.len()
                    && rest.is_char_boundary(name.len())
                    && rest[..name.len()].eq_ignore_ascii_case(name)
                    && matches!(rest.as_bytes()[name.len()], b'+' | b'-')
            });
            let Some(&(name, bit)) = hit else { break };
            mods |= bit;
            rest = &rest[name.len() + 1..];
        }
        debug_assert_eq!(mods & !0x0f, 0, "four modifier bits");
        Some(Chord::new(Mods(mods), Key::parse(rest)?))
    }

    /// Modifier bits.
    #[must_use]
    pub fn mods(self) -> Mods {
        Mods((self.0 >> 24) as u8)
    }

    /// Key code.
    #[must_use]
    pub fn key(self) -> Key {
        Key(self.0 & 0xff_ffff)
    }
}
