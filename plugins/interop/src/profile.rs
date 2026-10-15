//! Tool spellings as data (spec `interop.md` §3): coordinates, orientations and net/instance
//! names between Schemify and Virtuoso. Values and templates live with their tool
//! (`virtuoso::ael`, `xschem_out`), since each tool's table differs; everything here is
//! shared by both directions of the Virtuoso bridge.

use schemify_format::Orient;

/// Schemify milli-units per inch at Virtuoso's standard schematic resolution
/// (`DBUPerUU = 160`): one DBU is one xschem unit (§3.1).
pub const MILLI_PER_INCH: i64 = 160_000;

/// Virtuoso orientation of each Schemify orientation, indexed by `Orient as usize`
/// (`R0 R90 R180 R270 MR0 MR90 MR180 MR270`): the y flip turns clockwise into
/// counter-clockwise and swaps which mirror axis is which (§3.2).
pub const VIRTUOSO_ORIENT: [&str; 8] = ["R0", "R270", "R180", "R90", "MY", "MXR90", "MX", "MYR90"];

const ORIENTS: [Orient; 8] = [
    Orient::R0,
    Orient::R90,
    Orient::R180,
    Orient::R270,
    Orient::MR0,
    Orient::MR90,
    Orient::MR180,
    Orient::MR270,
];

/// The Virtuoso spelling of `o`.
pub fn orient_to_virtuoso(o: Orient) -> &'static str {
    VIRTUOSO_ORIENT[o as usize]
}

/// The Schemify orientation of a Virtuoso one; `None` for anything outside the eight
/// canonical names (older aliases such as `"0"` or `sideways` are not written by the dumper).
pub fn orient_from_virtuoso(s: &str) -> Option<Orient> {
    VIRTUOSO_ORIENT
        .iter()
        .position(|v| *v == s)
        .map(|k| ORIENTS[k])
}

/// A coordinate pair converted from Virtuoso units, and whether the conversion was exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Converted {
    /// Schemify milli-units, y down.
    pub p: [i32; 2],
    /// False when either coordinate was rounded (reported as `rounding`).
    pub exact: bool,
}

/// Converts a Virtuoso point in 1/`upi` inch (y up) to Schemify milli-units (y down).
/// `None` when the result leaves `i32`.
///
/// # Panics
///
/// `upi` must be positive (the dumper writes `round(DBUPerUU)`, never 0).
pub fn point_from_virtuoso(x: i32, y: i32, upi: i32) -> Option<Converted> {
    assert!(upi > 0, "units per inch must be positive, got {upi}");
    let conv = |v: i64| -> (i64, bool) {
        let n = v * MILLI_PER_INCH;
        let d = i64::from(upi);
        // Round half away from zero, exactly in integers.
        let q = (2 * n + d * n.signum()) / (2 * d);
        (q, n % d == 0)
    };
    let (cx, ex) = conv(i64::from(x));
    let (cy, ey) = conv(-i64::from(y));
    Some(Converted {
        p: [i32::try_from(cx).ok()?, i32::try_from(cy).ok()?],
        exact: ex && ey,
    })
}

/// Converts a Schemify point to the exporter's Virtuoso units (`upi` = [`MILLI_PER_INCH`]):
/// the same numbers with y negated. `None` only for `y == i32::MIN`.
pub fn point_to_virtuoso(p: [i32; 2]) -> Option<[i32; 2]> {
    Some([p[0], p[1].checked_neg()?])
}

/// True when milli-unit coordinate `v` lies on the DBU grid of a cellview with `dbu` database
/// units per inch (`off-grid` otherwise: Virtuoso would snap it).
pub fn on_dbu_grid(v: i32, dbu: i32) -> bool {
    assert!(dbu > 0, "DBUPerUU must be positive");
    (i64::from(v) * i64::from(dbu)) % MILLI_PER_INCH == 0
}

/// Text height per Schemify `size`, both in milli-units: `height = size × 10` (§3.5).
pub const HEIGHT_PER_SIZE_MILLI: i32 = 10;

/// The export height (milli-units) of a Schemify text `size` (milli): `size × 10`, rounded
/// half up to whole database units of 1/160 inch and at least one, as Virtuoso stores label
/// heights (live ICADVM20.1 run: size 0.18 came back 0.2, 0.25 came back 0.3). Saturates at
/// the largest whole-unit height an `i32` holds.
pub fn height_of_size(size_milli: i32) -> i32 {
    let dbu = MILLI_PER_INCH / 160;
    let h = i64::from(size_milli) * i64::from(HEIGHT_PER_SIZE_MILLI);
    let out = ((h + dbu / 2).div_euclid(dbu) * dbu).max(dbu);
    let max = i64::from(i32::MAX) / dbu * dbu;
    let out = i32::try_from(out.min(max)).expect("clamped into i32");
    debug_assert!(out >= 1000 && out % 1000 == 0);
    out
}

/// A dump text height (1/`upi` inch) in milli-units, the export's own units.
///
/// # Panics
///
/// `upi` must be positive (the reader refuses any other `cv`).
pub fn height_from_virtuoso(height: i32, upi: i32) -> i64 {
    assert!(upi > 0, "units per inch must be positive, got {upi}");
    i64::from(height) * MILLI_PER_INCH / i64::from(upi)
}

/// The Schemify `size` (milli) of a text `height_milli` high: a tenth, rounded half up
/// (`i32::MAX` past the range).
pub fn size_of_height(height_milli: i64) -> i32 {
    let out = i32::try_from((height_milli + 5) / 10).unwrap_or(i32::MAX);
    debug_assert!(height_milli < 0 || out >= 0);
    out
}

/// The box with corners `a` and `b` (any two opposite corners) as `[min x, min y, max x,
/// max y]`, the form both tools store a rectangle in.
pub fn norm_box(a: [i32; 2], b: [i32; 2]) -> [i32; 4] {
    let out = [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[0].max(b[0]),
        a[1].max(b[1]),
    ];
    debug_assert!(out[0] <= out[2] && out[1] <= out[3]);
    out
}

/// A milli-unit coordinate as Schemify text (the format's shortest decimal, `1.5`, `-20`).
pub fn coord_text(milli: i32) -> String {
    let mut s = String::new();
    schemify_format::format_coord(milli, &mut s);
    debug_assert!(!s.is_empty());
    s
}

// ---------------------------------------------------------------------------------------
// Directions
// ---------------------------------------------------------------------------------------

/// The Virtuoso terminal direction of a Schemify pin direction: `in` → `input`, `out` →
/// `output`, anything else → `inputOutput`.
pub fn dir_to_virtuoso(dir: &str) -> &'static str {
    match dir {
        "in" => "input",
        "out" => "output",
        _ => "inputOutput",
    }
}

/// The Schemify pin direction of a Virtuoso terminal direction: `input` → `in`, `output` →
/// `out`, anything else (`inputOutput`, `switch`, `jumper`, …) → `inout`.
pub fn dir_from_virtuoso(dir: &str) -> &'static str {
    match dir {
        "input" => "in",
        "output" => "out",
        _ => "inout",
    }
}

/// The `basic` pin master export draws a schematic pin of Virtuoso direction `dir` with
/// (`ipin`, `opin`, else `iopin`); import keeps any other master as `vpin`.
pub fn pin_master(dir: &str) -> &'static str {
    match dir {
        "input" => "ipin",
        "output" => "opin",
        _ => "iopin",
    }
}

// ---------------------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------------------

/// A Schemify net name with its globality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Net {
    /// The net expression in Schemify syntax.
    pub text: String,
    /// The label carries `global true`.
    pub global: bool,
    /// The spelling is outside what §3.3 maps exactly (reviewed as `name`).
    pub review: bool,
}

/// Longest name either tool spelling accepts here (Virtuoso allows 1024 characters).
pub const NAME_MAX: usize = 1024;

fn is_ground(s: &str) -> bool {
    s == "0" || s.eq_ignore_ascii_case("gnd")
}

/// Splits a Schemify base `ident[...]` into `(ident, Some(range text))`.
fn split_base(base: &str) -> (&str, Option<&str>) {
    match base.find('[') {
        Some(k) if base.ends_with(']') => (&base[..k], Some(&base[k + 1..base.len() - 1])),
        _ => (base, None),
    }
}

fn is_range(r: &str) -> bool {
    let parts: Vec<&str> = r.split(':').collect();
    (1..=2).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Writes a Schemify net expression (a label or port name) in Virtuoso syntax: `[a:b]` →
/// `<a:b>`, `n*t` → `<*n>t`, a global label's name gains `!` (before any range), and the
/// ground names `0`/`gnd` (any case) of a global or `0` become `gnd!`.
///
/// Expressions outside netlist.md §4's grammar are returned verbatim with `review` set.
pub fn net_to_virtuoso(text: &str, global: bool) -> (String, bool) {
    if text == "0" || (global && is_ground(text)) {
        return ("gnd!".into(), false);
    }
    let mut out = String::new();
    let mut review = false;
    for (k, term) in text.split(',').enumerate() {
        if k > 0 {
            out.push(',');
        }
        let (count, base) = match term.split_once('*') {
            Some((n, b)) if !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) => (Some(n), b),
            _ => (None, term),
        };
        let (ident, range) = split_base(base);
        let ok_ident = !ident.is_empty()
            && !ident.contains(['<', '>', '(', ')', ',', '/', ' ', '[', ']', '*', ':']);
        if !ok_ident || range.is_some_and(|r| !is_range(r)) {
            review = true;
        }
        if let Some(n) = count {
            out.push_str("<*");
            out.push_str(n);
            out.push('>');
        }
        out.push_str(ident);
        if global && !ident.ends_with('!') {
            out.push('!');
        }
        if let Some(r) = range {
            out.push('<');
            out.push_str(r);
            out.push('>');
        }
    }
    (out, review)
}

/// One expanded Virtuoso bit: base name and optional index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bit {
    /// Base name, `!` included.
    pub base: String,
    /// Bus index, if any.
    pub index: Option<u32>,
}

/// Expands a Virtuoso name expression into its bits, in order (the semantics of
/// `dbProduceMemName`: `<*n>` binds to the following term or parenthesised group, `*n`
/// inside `<>` repeats the preceding vector term per bit, ranges run in written order with
/// an optional positive step). `None` for text outside the grammar.
pub fn expand_virtuoso(s: &str) -> Option<Vec<Bit>> {
    let mut p = NameParser {
        s: s.as_bytes(),
        i: 0,
    };
    let bits = p.name_expr()?;
    if p.i != p.s.len() || bits.len() > 65_535 {
        return None;
    }
    Some(bits)
}

struct NameParser<'a> {
    s: &'a [u8],
    i: usize,
}

impl NameParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn num(&mut self) -> Option<u32> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if start == self.i {
            return None;
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()?
            .parse()
            .ok()
            .filter(|n| *n < 65_536)
    }

    fn name_expr(&mut self) -> Option<Vec<Bit>> {
        let mut out = Vec::new();
        loop {
            let mut rep = 1;
            if self.peek() == Some(b'<') && self.s.get(self.i + 1) == Some(&b'*') {
                self.i += 2;
                rep = self.num()?;
                if self.peek() != Some(b'>') {
                    return None;
                }
                self.i += 1;
            }
            let term = self.name_term()?;
            for _ in 0..rep {
                out.extend(term.iter().cloned());
            }
            if self.peek() == Some(b',') {
                self.i += 1;
                continue;
            }
            return Some(out);
        }
    }

    fn name_term(&mut self) -> Option<Vec<Bit>> {
        if self.peek() == Some(b'(') {
            self.i += 1;
            let inner = self.name_expr()?;
            if self.peek() != Some(b')') {
                return None;
            }
            self.i += 1;
            return Some(inner);
        }
        let start = self.i;
        while let Some(c) = self.peek() {
            if matches!(c, b'<' | b'>' | b'(' | b')' | b',' | b'/' | b'*' | b':')
                || c.is_ascii_whitespace()
            {
                break;
            }
            self.i += 1;
        }
        if start == self.i {
            return None;
        }
        let base = std::str::from_utf8(&self.s[start..self.i]).ok()?.to_owned();
        if self.peek() != Some(b'<') {
            return Some(vec![Bit { base, index: None }]);
        }
        self.i += 1;
        let idx = self.vect_expr()?;
        if self.peek() != Some(b'>') {
            return None;
        }
        self.i += 1;
        Some(
            idx.into_iter()
                .map(|n| Bit {
                    base: base.clone(),
                    index: Some(n),
                })
                .collect(),
        )
    }

    fn vect_expr(&mut self) -> Option<Vec<u32>> {
        let mut out = Vec::new();
        loop {
            let term = self.vect_term()?;
            let mut rep = 1;
            if self.peek() == Some(b'*') {
                self.i += 1;
                rep = self.num()?;
            }
            for n in &term {
                for _ in 0..rep {
                    out.push(*n);
                }
            }
            if self.peek() == Some(b',') {
                self.i += 1;
                continue;
            }
            return Some(out);
        }
    }

    fn vect_term(&mut self) -> Option<Vec<u32>> {
        if self.peek() == Some(b'(') {
            self.i += 1;
            let inner = self.vect_expr()?;
            if self.peek() != Some(b')') {
                return None;
            }
            self.i += 1;
            // `(0:2)*2` repeats the sequence: handled by the caller seeing `*` after `)`.
            let mut rep = 1;
            if self.peek() == Some(b'*') {
                self.i += 1;
                rep = self.num()?;
            }
            let mut out = Vec::new();
            for _ in 0..rep {
                out.extend(inner.iter().copied());
            }
            return Some(out);
        }
        let a = self.num()?;
        if self.peek() != Some(b':') {
            return Some(vec![a]);
        }
        self.i += 1;
        let b = self.num()?;
        let mut step = 1;
        if self.peek() == Some(b':') {
            self.i += 1;
            step = self.num().filter(|s| *s > 0)?;
        }
        let mut out = Vec::new();
        if a <= b {
            let mut k = a;
            while k <= b {
                out.push(k);
                k += step;
            }
        } else {
            let mut k = i64::from(a);
            while k >= i64::from(b) {
                out.push(u32::try_from(k).ok()?);
                k -= i64::from(step);
            }
        }
        Some(out)
    }
}

/// Reads a Virtuoso net name into Schemify syntax (§3.3): `gnd!` → global `GND`; simple
/// buses and repeats map term by term (`d<3:0>` → `d[3:0]`, `<*2>a` → `2*a`); anything using
/// steps, suffix repeats or grouping expands to an explicit bundle, runs of consecutive bits
/// written as ranges; text outside the grammar stays verbatim with `review`.
pub fn net_from_virtuoso(s: &str) -> Net {
    if s == "gnd!" {
        return Net {
            text: "GND".into(),
            global: true,
            review: false,
        };
    }
    // Simple forms first: terms of `[<*n>]base[<a[:b]>]`, no nesting.
    if let Some(text) = simple_from_virtuoso(s) {
        return Net {
            text,
            global: false,
            review: false,
        };
    }
    match expand_virtuoso(s) {
        Some(bits) => Net {
            text: compress(&bits),
            global: false,
            review: false,
        },
        None => Net {
            text: s.to_owned(),
            global: false,
            review: true,
        },
    }
}

fn simple_from_virtuoso(s: &str) -> Option<String> {
    let mut out = String::new();
    for (k, term) in s.split(',').enumerate() {
        if k > 0 {
            out.push(',');
        }
        let mut t = term;
        if let Some(rest) = t.strip_prefix("<*") {
            let (n, rest) = rest.split_once('>')?;
            if n.is_empty() || !n.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            out.push_str(n);
            out.push('*');
            t = rest;
        }
        let (base, range) = match t.find('<') {
            Some(i) => {
                let r = t[i + 1..].strip_suffix('>')?;
                (&t[..i], Some(r))
            }
            None => (t, None),
        };
        if base.is_empty() || base.contains(['(', ')', '<', '>', '*', ':', '/']) {
            return None;
        }
        out.push_str(base);
        if let Some(r) = range {
            if !is_range(r) {
                return None;
            }
            out.push('[');
            out.push_str(r);
            out.push(']');
        }
    }
    Some(out)
}

/// Writes bits as a Schemify bundle, consecutive same-base runs (step ±1) as ranges.
fn compress(bits: &[Bit]) -> String {
    let mut out = String::new();
    let mut k = 0;
    while k < bits.len() {
        if !out.is_empty() {
            out.push(',');
        }
        let b = &bits[k];
        out.push_str(&b.base);
        let Some(first) = b.index else {
            k += 1;
            continue;
        };
        // Extend the run.
        let mut end = k;
        let dir: i64 = match bits.get(k + 1) {
            Some(n) if n.base == b.base && n.index.is_some() => {
                i64::from(n.index.unwrap()) - i64::from(first)
            }
            _ => 0,
        };
        if dir == 1 || dir == -1 {
            while let Some(n) = bits.get(end + 1) {
                match n.index {
                    Some(i)
                        if n.base == b.base
                            && i64::from(i) - i64::from(bits[end].index.unwrap()) == dir =>
                    {
                        end += 1
                    }
                    _ => break,
                }
            }
        }
        if end > k {
            out.push_str(&format!("[{}:{}]", first, bits[end].index.unwrap()));
        } else {
            out.push_str(&format!("[{first}]"));
        }
        k = end + 1;
    }
    out
}

/// An instance name in Virtuoso syntax: `X1[3:0]` → `X1<3:0>`, scalar names unchanged.
/// `None` for a name Virtuoso cannot hold (step or bundle syntax, `/`).
pub fn inst_to_virtuoso(name: &str) -> Option<String> {
    let (base, range) = split_base(name);
    if base.is_empty() || base.contains(['<', '>', ',', '(', ')', '*', ':', '/', ' ', '[', ']']) {
        return None;
    }
    match range {
        None => Some(base.to_owned()),
        Some(r) if is_range(r) => Some(format!("{base}<{r}>")),
        Some(_) => None,
    }
}

/// A Virtuoso instance name in Schemify syntax: `I0<3:0>` → `I0[3:0]`.
pub fn inst_from_virtuoso(name: &str) -> String {
    match name.find('<') {
        Some(i) if name.ends_with('>') && is_range(&name[i + 1..name.len() - 1]) => {
            format!("{}[{}]", &name[..i], &name[i + 1..name.len() - 1])
        }
        _ => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Schemify transform (format spec §3), y down.
    fn schemify_map(o: Orient, p: [i64; 2]) -> [i64; 2] {
        let (mut x, y) = (p[0], p[1]);
        if o.mirrored() {
            x = -x;
        }
        match o.quarter_turns() {
            0 => [x, y],
            1 => [-y, x],
            2 => [-x, -y],
            _ => [y, -x],
        }
    }

    /// Virtuoso transform (research §7), y up: mirror first, then counter-clockwise turns.
    fn virtuoso_map(o: &str, p: [i64; 2]) -> [i64; 2] {
        let [x, y] = p;
        match o {
            "R0" => [x, y],
            "R90" => [-y, x],
            "R180" => [-x, -y],
            "R270" => [y, -x],
            "MY" => [-x, y],
            "MYR90" => [-y, -x],
            "MX" => [x, -y],
            "MXR90" => [y, x],
            _ => unreachable!(),
        }
    }

    #[test]
    fn orientation_table_is_the_y_flip_conjugate() {
        for o in ORIENTS {
            let v = orient_to_virtuoso(o);
            assert_eq!(orient_from_virtuoso(v), Some(o));
            for p in [[3, 7], [-5, 2], [0, 1]] {
                // Schemify point → flip to y-up → Virtuoso map → flip back.
                let up = virtuoso_map(v, [p[0], -p[1]]);
                assert_eq!(schemify_map(o, p), [up[0], -up[1]], "{o:?} ↔ {v}");
            }
        }
    }

    #[test]
    fn coordinates_round_trip_and_report_rounding() {
        let c = point_from_virtuoso(10, -20, 160).unwrap();
        assert_eq!(
            c,
            Converted {
                p: [10_000, 20_000],
                exact: true
            }
        );
        let back = point_to_virtuoso([10_000, 20_000]).unwrap();
        assert_eq!(
            point_from_virtuoso(back[0], back[1], 160_000).unwrap().p,
            [10_000, 20_000]
        );
        let r = point_from_virtuoso(1, 0, 3).unwrap();
        assert!(!r.exact);
        assert_eq!(r.p, [53_333, 0]);
        assert!(on_dbu_grid(1000, 160) && !on_dbu_grid(500, 160));
    }

    #[test]
    fn net_names_map_both_ways() {
        assert_eq!(net_to_virtuoso("d[3:0]", false), ("d<3:0>".into(), false));
        assert_eq!(
            net_to_virtuoso("2*a,b[1:0]", false),
            ("<*2>a,b<1:0>".into(), false)
        );
        assert_eq!(net_to_virtuoso("VDD", true), ("VDD!".into(), false));
        assert_eq!(net_to_virtuoso("GND", true).0, "gnd!");
        assert_eq!(net_to_virtuoso("0", false).0, "gnd!");
        assert_eq!(net_to_virtuoso("D[3:0]", true).0, "D!<3:0>");
        assert!(net_to_virtuoso("a<b", false).1);
        for (v, s) in [
            ("d<3:0>", "d[3:0]"),
            ("<*2>a,b<1:0>", "2*a,b[1:0]"),
            ("x!", "x!"),
            ("n1", "n1"),
        ] {
            assert_eq!(net_from_virtuoso(v).text, s);
            assert_eq!(net_to_virtuoso(s, false).0, v);
        }
        assert_eq!(
            net_from_virtuoso("gnd!"),
            Net {
                text: "GND".into(),
                global: true,
                review: false
            }
        );
        assert_eq!(net_from_virtuoso("b<3:0:2>").text, "b[3],b[1]");
        assert_eq!(net_from_virtuoso("<*2>(a,b),c").text, "a,b,a,b,c");
        assert_eq!(
            net_from_virtuoso("A<0:2*2>").text,
            "A[0],A[0:1],A[1:2],A[2]"
        ); // same bits, runs compressed
        assert_eq!(net_from_virtuoso("A<(0:2)*2>").text, "A[0:2],A[0:2]");
        assert!(net_from_virtuoso("x(a)").review);
    }

    #[test]
    fn expansion_matches_the_documented_examples() {
        let e = |s: &str| -> Vec<String> {
            expand_virtuoso(s)
                .unwrap()
                .iter()
                .map(|b| match b.index {
                    Some(i) => format!("{}<{i}>", b.base),
                    None => b.base.clone(),
                })
                .collect()
        };
        assert_eq!(
            e("DATA<1:7:2>"),
            ["DATA<1>", "DATA<3>", "DATA<5>", "DATA<7>"]
        );
        assert_eq!(e("<*2>A,B,C"), ["A", "A", "B", "C"]);
        assert_eq!(e("<*2>(A,<*2>(X,Y)),B").len(), 11);
        assert_eq!(
            e("A<2:0>,B,C<3:4>"),
            ["A<2>", "A<1>", "A<0>", "B", "C<3>", "C<4>"]
        );
        assert_eq!(e("b<0,1:3:4*1,2:2>"), ["b<0>", "b<1>", "b<2>"]);
    }

    #[test]
    fn directions_map_both_ways_and_default_to_inout() {
        for d in ["in", "out", "inout"] {
            assert_eq!(dir_from_virtuoso(dir_to_virtuoso(d)), d);
        }
        // Negative space: every other spelling is bidirectional on both sides.
        for other in ["", "IN", "input ", "switch", "jumper", "unknown"] {
            assert_eq!(dir_to_virtuoso(other), "inputOutput", "{other:?}");
            assert_eq!(dir_from_virtuoso(other), "inout", "{other:?}");
            assert_eq!(pin_master(other), "iopin", "{other:?}");
        }
        assert_eq!(
            ["input", "output", "inputOutput"].map(pin_master),
            ["ipin", "opin", "iopin"]
        );
    }

    #[test]
    fn coordinate_text_is_the_format_spelling() {
        for (v, t) in [(0, "0"), (1500, "1.5"), (-20_000, "-20"), (156, "0.156")] {
            assert_eq!(coord_text(v), t);
            assert_eq!(schemify_format::parse_coord(t), Ok(v));
        }
    }

    proptest::proptest! {
        /// A box is the same whichever two opposite corners name it, and holds both.
        #[test]
        fn boxes_do_not_depend_on_corner_order(a: [i32; 2], b: [i32; 2]) {
            let n = norm_box(a, b);
            proptest::prop_assert_eq!(n, norm_box(b, a));
            proptest::prop_assert_eq!(n, norm_box([a[0], b[1]], [b[0], a[1]]));
            for p in [a, b] {
                proptest::prop_assert!(n[0] <= p[0] && p[0] <= n[2] && n[1] <= p[1] && p[1] <= n[3]);
            }
        }
    }

    /// The export height of a size against a float reference: ×10, rounded half up to
    /// whole 1/160 inch (1000 milli), at least one unit.
    #[test]
    fn text_heights_round_to_whole_database_units() {
        for size in -2_000..=200_000 {
            let h = f64::from(size) * 10.0 / 1000.0;
            let want = ((h + 0.5).floor() * 1000.0).max(1000.0) as i32;
            assert_eq!(height_of_size(size), want, "size {size}");
        }
        // The live observations (ICADVM20.1): 0.18 → 0.2, 0.25 → 0.3.
        assert_eq!(size_of_height(i64::from(height_of_size(180))), 200);
        assert_eq!(size_of_height(i64::from(height_of_size(250))), 300);
        // Import of an export: the size rounded half up to 0.1, never below it.
        for size in 0..=50_000 {
            let back = size_of_height(i64::from(height_of_size(size)));
            assert_eq!(back, ((size + 50) / 100 * 100).max(100), "size {size}");
        }
    }

    /// Sizes whose height leaves `i32` saturate at the largest whole-unit height instead of
    /// overflowing (a `size 300000` attribute panicked the export in debug builds).
    #[test]
    fn huge_text_sizes_saturate() {
        let top = i32::MAX / 1000 * 1000;
        for size in [i32::MAX / 10, i32::MAX / 10 + 1, 300_000_000, i32::MAX] {
            assert_eq!(height_of_size(size), top, "size {size}");
        }
        assert_eq!(height_of_size(i32::MIN), 1000);
        assert_eq!(size_of_height(i64::MAX / 2), i32::MAX);
    }

    #[test]
    fn heights_read_in_milli_units() {
        // 1/16 inch at 160 DBU per inch and at the exporter's own resolution.
        assert_eq!(height_from_virtuoso(10, 160), 10_000);
        assert_eq!(height_from_virtuoso(10_000, 160_000), 10_000);
        assert_eq!(size_of_height(height_from_virtuoso(10, 160)), 1000);
    }

    #[test]
    #[should_panic(expected = "units per inch must be positive")]
    fn heights_need_a_resolution() {
        height_from_virtuoso(10, 0);
    }

    #[test]
    fn instance_names() {
        assert_eq!(inst_to_virtuoso("X1[3:0]").as_deref(), Some("X1<3:0>"));
        assert_eq!(inst_from_virtuoso("I0<3:0>"), "I0[3:0]");
        assert_eq!(inst_to_virtuoso("a,b"), None);
    }
}
