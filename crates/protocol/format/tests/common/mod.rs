//! Seeded generators shared by the integration tests: a u64 seed → values, ids,
//! coordinates, documents (`docgen.rs`) and noisy re-spellings of canonical text.
//! Everything derives from one seed (proptest supplies it), so any failure replays from
//! the printed seed.
//!
//! Contents:
//! - `pub struct Rng`, `Rng::{new, next, below, chance, pick, word, shuffle}`
//! - `pub fn value`, `pub fn unique_value`, `pub fn verbatim_line`, `pub fn comment`,
//!   `pub fn new_id`, `pub fn coord`, `pub fn orient`
//! - `pub mod docgen` (`gen_doc`)
//! - `pub fn chunks`, `pub fn chunk_word`, `pub fn messy`

#![allow(dead_code, reason = "each test binary uses a subset")]

pub mod docgen;

use schemify_format as format;
use schemify_format::{IdRng, Orient};
use std::collections::BTreeSet;

/// splitmix64.
pub struct Rng(u64);

/// Pieces that stress quoting: whitespace, quotes, escapes, controls, `#`/`|` starts, Unicode.
const FRAGMENTS: &[&str] = &[
    "a",
    "W",
    "1u",
    "0.5",
    "-3",
    " ",
    "  ",
    "\"",
    "\\",
    "\n",
    "\t",
    "#",
    "|",
    "µ",
    "{name}",
    "x y",
    "\r",
    "\u{0}",
    "\u{7f}",
    "é",
    "=",
    "(",
    "nfet_01v8",
    "\u{1f600}",
    "\\n",
    "u{41}",
];

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0);
        self.next() % n
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }

    pub fn word(&mut self, xs: &[&'static str]) -> &'static str {
        xs[self.below(xs.len() as u64) as usize]
    }

    pub fn shuffle<T>(&mut self, xs: &mut [T]) {
        for i in (1..xs.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            xs.swap(i, j);
        }
    }
}

/// A value of 0–3 fragments (so possibly empty).
pub fn value(rng: &mut Rng) -> String {
    (0..rng.below(4)).map(|_| *rng.pick(FRAGMENTS)).collect()
}

/// A value guaranteed distinct from earlier ones of the same set.
pub fn unique_value(rng: &mut Rng, used: &mut BTreeSet<String>) -> String {
    loop {
        let v = format!("{}{}", value(rng), rng.below(1000));
        if used.insert(v.clone()) {
            return v;
        }
    }
}

/// Text for a verbatim line: anything but CR/LF; may have leading/trailing spaces.
pub fn verbatim_line(rng: &mut Rng) -> String {
    value(rng).replace(['\n', '\r'], "~")
}

/// Text after `#`: no CR/LF, no trailing whitespace.
pub fn comment(rng: &mut Rng) -> String {
    let t = value(rng).replace(['\n', '\r'], "~");
    t.trim_end_matches([' ', '\t']).to_string()
}

pub fn new_id(rng: &mut Rng, used: &mut BTreeSet<String>) -> String {
    let mut ids = IdRng::new(rng.next());
    let id = format::fresh_id(&mut ids, |c| used.contains(c))
        .as_str()
        .to_string();
    used.insert(id.clone());
    id
}

/// Milli-unit coordinate: mostly on a 10-unit grid, sometimes any representable value.
pub fn coord(rng: &mut Rng) -> i32 {
    if rng.chance(80) {
        (rng.below(200) as i32 - 100) * 10_000
    } else {
        rng.next() as i32
    }
}

pub fn orient(rng: &mut Rng) -> Orient {
    *rng.pick(&[
        Orient::R0,
        Orient::R90,
        Orient::R180,
        Orient::R270,
        Orient::MR0,
        Orient::MR90,
        Orient::MR180,
        Orient::MR270,
    ])
}

/// Splits canonical text into chunks (comments + record line + indented lines) and
/// returns them without blank lines.
pub fn chunks(text: &str) -> Vec<Vec<&str>> {
    let mut out: Vec<Vec<&str>> = Vec::new();
    let mut pending: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            pending.push(line);
        } else if line.starts_with(' ') {
            out.last_mut()
                .expect("indented line after a record")
                .push(line);
        } else {
            pending.push(line);
            out.push(std::mem::take(&mut pending));
        }
    }
    if !pending.is_empty() {
        out.push(pending);
    }
    out
}

/// The record word of a chunk (first non-comment line).
pub fn chunk_word<'a>(chunk: &[&'a str]) -> &'a str {
    let line = chunk
        .iter()
        .find(|l| !l.starts_with('#'))
        .copied()
        .unwrap_or("");
    line.split(' ').next().unwrap_or("")
}

/// Re-spaces record and attribute lines outside quotes, indents attributes differently,
/// adds trailing whitespace and switches some line ends to CRLF. Verbatim and comment
/// lines keep their content (only comment trailing whitespace changes).
pub fn messy(rng: &mut Rng, text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let (indent, body) = match line.strip_prefix("  ") {
            Some(b) => (true, b),
            None => (false, line),
        };
        if body.starts_with('|') {
            out.push_str(rng.word(&["  ", "\t", "    "]));
            out.push_str(body);
        } else if line.starts_with('#') || line.is_empty() {
            out.push_str(rng.word(&["", "  "]));
            out.push_str(line);
            if line.starts_with('#') {
                out.push_str(rng.word(&["", " ", "\t "]));
            }
        } else {
            if indent {
                out.push_str(rng.word(&["  ", "\t", "      ", " \t"]));
            }
            let (mut quoted, mut escaped) = (false, false);
            for c in body.chars() {
                if !quoted && c == ' ' {
                    out.push_str(rng.word(&[" ", "  ", "\t", " \t "]));
                    continue;
                }
                out.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    quoted = !quoted;
                }
            }
            out.push_str(rng.word(&["", " ", "\t"]));
        }
        out.push_str(rng.word(&["\n", "\r\n"]));
        if rng.chance(10) {
            out.push('\n');
        }
    }
    out
}
