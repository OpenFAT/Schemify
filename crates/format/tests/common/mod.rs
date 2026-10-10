//! Seeded generators shared by the integration tests. Everything derives from one u64
//! seed (proptest supplies it), so any failure replays from the printed seed.

#![allow(dead_code)] // each test binary uses a subset

use schemify_format::{Document, FileKind, IdRng, Orient, RecordKind, fresh_id};
use std::collections::BTreeSet;

/// splitmix64.
pub struct Rng(u64);

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
    "\u{1F600}",
    "\\n",
    "u{41}",
];

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
    let id = fresh_id(&mut ids, |c| used.contains(c))
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

fn maybe_comment(rng: &mut Rng, d: &mut Document<'_>) {
    for _ in 0..(if rng.chance(20) { rng.below(3) } else { 0 }) {
        d.push_comment(&comment(rng));
    }
}

fn attr(d: &mut Document<'_>, key: &str, values: &[&str]) {
    d.push_attr(key);
    for v in values {
        d.push_value(v);
    }
}

/// An attribute word unknown to every closed record schema (written after known ones).
fn unknown_attr(rng: &mut Rng, d: &mut Document<'_>) {
    if rng.chance(15) {
        let key = rng.word(&["zz-extra", "x.future", "v2:hint"]).to_string();
        let v = value(rng);
        attr(d, &key, &[&v]);
    }
}

/// A random, valid document in canonical model order, so `parse(write(d)) == d`.
pub fn gen_doc(rng: &mut Rng, kind: FileKind) -> Document<'static> {
    assert!(matches!(kind, FileKind::Shcomp | FileKind::Shbench));
    let mut d = Document::new(kind);
    let two_impls = kind == FileKind::Shcomp && rng.chance(40);
    if rng.chance(50) {
        let v = value(rng);
        attr(&mut d, "doc", &[&v]);
    }
    if two_impls {
        attr(&mut d, "default", &["sch"]);
    }
    if rng.chance(30) {
        attr(&mut d, "pdk", &["sky130"]);
    }
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    if kind == FileKind::Shcomp {
        symbol(rng, &mut d, two_impls, &mut names);
    }
    let insts = schematic(rng, &mut d, kind, &mut ids);
    docs(rng, &mut d, &mut ids);
    if two_impls {
        d.begin_record(RecordKind::Body);
        maybe_comment(rng, &mut d);
        d.push_field("sp");
        for _ in 0..rng.below(3) {
            d.push_verbatim(&verbatim_line(rng));
        }
    }
    if kind == FileKind::Shbench {
        bench(rng, &mut d);
    }
    let _ = insts;
    for _ in 0..rng.below(3) {
        d.push_eof_comment(&comment(rng));
    }
    d
}

fn symbol(rng: &mut Rng, d: &mut Document<'_>, two_impls: bool, names: &mut BTreeSet<String>) {
    // Rank 1 is file order, so pins, params and impls may interleave.
    let mut items: Vec<u8> = vec![0; 1 + rng.below(4) as usize];
    items.extend(vec![1; rng.below(3) as usize]);
    items.push(2);
    if two_impls {
        items.push(3);
    }
    rng.shuffle(&mut items);
    for item in items {
        match item {
            0 => {
                d.begin_record(RecordKind::Pin);
                maybe_comment(rng, d);
                let name = unique_value(rng, names);
                d.push_field(&name);
                d.push_field(rng.word(&["in", "out", "inout"]));
                d.push_field_coord(coord(rng));
                d.push_field_coord(coord(rng));
                if rng.chance(30) {
                    attr(d, "domain", &[rng.word(&["analog", "digital", "real"])]);
                }
                if rng.chance(20) {
                    attr(d, "number", &["1"]);
                }
                unknown_attr(rng, d);
            }
            1 => {
                d.begin_record(RecordKind::Param);
                maybe_comment(rng, d);
                let name = unique_value(rng, names);
                let default = value(rng);
                d.push_field(&name);
                d.push_field(&default);
                if rng.chance(40) {
                    attr(
                        d,
                        "kind",
                        &[rng.word(&["number", "int", "bool", "string", "choice"])],
                    );
                }
                if rng.chance(30) {
                    let v = value(rng);
                    attr(d, "doc", &[&v]);
                }
                if rng.chance(30) {
                    let (x, y) = (value(rng), value(rng));
                    attr(d, "choices", &[&x, &y, "c"]);
                }
                unknown_attr(rng, d);
            }
            2 => {
                d.begin_record(RecordKind::Impl);
                d.push_field("sch");
                d.push_field("schematic");
            }
            _ => {
                d.begin_record(RecordKind::Impl);
                maybe_comment(rng, d);
                d.push_field("sp");
                d.push_field("spice");
                let t = value(rng);
                attr(d, "format", &["spice", &t]);
                attr(d, "format", &["cdl", "X{name}"]);
                attr(d, "model", &["nfet"]);
            }
        }
    }
    for _ in 0..rng.below(5) {
        let kind = *rng.pick(&[
            RecordKind::Line,
            RecordKind::Rect,
            RecordKind::Arc,
            RecordKind::Circle,
            RecordKind::Poly,
            RecordKind::Stext,
        ]);
        d.begin_record(kind);
        maybe_comment(rng, d);
        let coords = match kind {
            RecordKind::Line | RecordKind::Rect => 4,
            RecordKind::Arc | RecordKind::Circle => 3,
            RecordKind::Poly => 4 + 2 * rng.below(3) as usize,
            _ => 2,
        };
        for _ in 0..coords {
            d.push_field_coord(coord(rng));
        }
        match kind {
            RecordKind::Arc => {
                d.push_field("0");
                d.push_field("-90.5");
            }
            RecordKind::Stext => {
                d.push_field(orient(rng).as_str());
                let t = value(rng);
                d.push_field(&t);
                if rng.chance(50) {
                    attr(d, "size", &["0.2"]);
                }
                if rng.chance(30) {
                    attr(d, "hcenter", &[]);
                }
            }
            _ => {}
        }
        if rng.chance(40) {
            attr(d, "layer", &["4"]);
        }
        if kind != RecordKind::Line && kind != RecordKind::Stext && rng.chance(30) {
            attr(d, "fill", &[rng.word(&["true", "false"])]);
        }
        unknown_attr(rng, d);
    }
}

/// Pushes rank-3 records, each kind sorted by id; returns the instance ids.
fn schematic(
    rng: &mut Rng,
    d: &mut Document<'_>,
    kind: FileKind,
    ids: &mut BTreeSet<String>,
) -> Vec<String> {
    let mut by_kind: Vec<(RecordKind, Vec<String>)> = Vec::new();
    let kinds: &[RecordKind] = if kind == FileKind::Shcomp {
        &[
            RecordKind::Inst,
            RecordKind::Wire,
            RecordKind::Label,
            RecordKind::Port,
            RecordKind::Noconn,
            RecordKind::Text,
            RecordKind::Constraint,
            RecordKind::Waive,
        ]
    } else {
        &[
            RecordKind::Inst,
            RecordKind::Wire,
            RecordKind::Label,
            RecordKind::Noconn,
            RecordKind::Text,
            RecordKind::Constraint,
            RecordKind::Waive,
        ]
    };
    for &k in kinds {
        let n = rng.below(if k == RecordKind::Inst || k == RecordKind::Wire {
            6
        } else {
            3
        }) as usize;
        let mut v: Vec<String> = (0..n).map(|_| new_id(rng, ids)).collect();
        v.sort();
        by_kind.push((k, v));
    }
    let insts = by_kind[0].1.clone();
    for (k, list) in &by_kind {
        for id in list {
            d.begin_record(*k);
            maybe_comment(rng, d);
            d.push_field(id);
            match k {
                RecordKind::Inst => {
                    let cell = rng
                        .word(&["sky130:nfet_01v8", "./ota5t", "core:res"])
                        .to_string();
                    d.push_field(&cell);
                    d.push_attr("at");
                    d.push_value_coord(coord(rng));
                    d.push_value_coord(coord(rng));
                    d.push_value(orient(rng).as_str());
                    let name = value(rng);
                    attr(d, "name", &[&name]);
                    let mut keys = vec!["W", "L", "nf", "m", "model", "x.y", "a-b:c"];
                    rng.shuffle(&mut keys);
                    for key in &keys[..rng.below(4) as usize] {
                        let v = value(rng);
                        attr(d, key, &[&v]);
                    }
                    if rng.chance(20) {
                        attr(d, "inherit", &["VDD", "vdd!"]);
                        attr(d, "inherit", &["VSS", "gnd"]);
                    }
                }
                RecordKind::Wire => {
                    let (x1, y1) = (coord(rng), coord(rng));
                    let (mut x2, y2) = (coord(rng), coord(rng));
                    if (x1, y1) == (x2, y2) {
                        x2 = x2.wrapping_add(1);
                    }
                    let (a, b) = if (x2, y2) < (x1, y1) {
                        ((x2, y2), (x1, y1))
                    } else {
                        ((x1, y1), (x2, y2))
                    };
                    for c in [a.0, a.1, b.0, b.1] {
                        d.push_field_coord(c);
                    }
                    if rng.chance(20) {
                        attr(d, "bus", &["true"]);
                    }
                }
                RecordKind::Label | RecordKind::Port | RecordKind::Text => {
                    d.push_field_coord(coord(rng));
                    d.push_field_coord(coord(rng));
                    d.push_field(orient(rng).as_str());
                    let t = value(rng);
                    d.push_field(&t);
                    if *k == RecordKind::Text && rng.chance(30) {
                        attr(d, "size", &["0.4"]);
                    }
                }
                RecordKind::Noconn => {
                    d.push_field_coord(coord(rng));
                    d.push_field_coord(coord(rng));
                }
                RecordKind::Constraint => {
                    d.push_field(rng.word(&["match", "symmetry", "plugin.thing"]));
                    for m in insts.iter().take(rng.below(3) as usize) {
                        attr(d, "member", &[m]);
                    }
                    if rng.chance(50) {
                        let v = value(rng);
                        attr(d, "tolerance", &[&v]);
                    }
                }
                _ => {
                    let target = insts.first().cloned().unwrap_or_else(|| "0".to_string());
                    d.push_field(&target);
                    d.push_field("W");
                    let reason = value(rng);
                    attr(d, "reason", &[&reason]);
                }
            }
        }
    }
    insts
}

fn docs(rng: &mut Rng, d: &mut Document<'_>, ids: &mut BTreeSet<String>) {
    let mut vars = BTreeSet::new();
    for _ in 0..rng.below(4) {
        if rng.chance(50) {
            d.begin_record(RecordKind::Prose);
            maybe_comment(rng, d);
            let id = new_id(rng, ids);
            d.push_field(&id);
            for _ in 0..rng.below(4) {
                d.push_verbatim(&verbatim_line(rng));
            }
        } else {
            d.begin_record(RecordKind::Var);
            maybe_comment(rng, d);
            let name = unique_value(rng, &mut vars);
            let expr = value(rng);
            d.push_field(&name);
            d.push_field(&expr);
            if rng.chance(40) {
                attr(d, "unit", &["um"]);
            }
            unknown_attr(rng, d);
        }
    }
}

fn bench(rng: &mut Rng, d: &mut Document<'_>) {
    let kinds = [
        RecordKind::Corner,
        RecordKind::Config,
        RecordKind::Sweep,
        RecordKind::Montecarlo,
        RecordKind::Optimize,
        RecordKind::Output,
        RecordKind::Spec,
        RecordKind::Code,
    ];
    for k in kinds {
        let mut used = BTreeSet::new();
        let mut names: Vec<String> = (0..rng.below(3))
            .map(|_| unique_value(rng, &mut used))
            .collect();
        names.sort();
        for name in names {
            d.begin_record(k);
            maybe_comment(rng, d);
            d.push_field(&name);
            match k {
                RecordKind::Corner => {
                    attr(d, "lib", &["models.lib", "tt"]);
                    attr(d, "lib", &["models.lib", "mos_tt"]);
                    attr(d, "set", &["vdd", "1.8"]);
                    attr(d, "temp", &["27"]);
                }
                RecordKind::Config => attr(d, "order", &["sch", "sp"]),
                RecordKind::Sweep => {
                    attr(d, "var", &["vdd"]);
                    attr(d, "lin", &["1", "2", "11"]);
                }
                RecordKind::Montecarlo => attr(d, "samples", &["100"]),
                RecordKind::Optimize => {
                    attr(d, "goal", &["gain", "max"]);
                    attr(d, "goal", &["power", "target", "1m"]);
                }
                RecordKind::Output => {
                    let e = value(rng);
                    d.push_field(&e);
                }
                RecordKind::Spec => attr(d, "min", &["60"]),
                _ => {
                    d.push_field(rng.word(&["spice", "ngspice", "python"]));
                    for _ in 0..rng.below(4) {
                        d.push_verbatim(&verbatim_line(rng));
                    }
                }
            }
        }
    }
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
