//! An extracted netlist → what `schemify annotate` writes from it: its `.subckt`s and the
//! devices it calls ([`scan`]), the one the cell names ([`pick`]), its ports checked against
//! the cell's port bits by name ([`port_bits`], [`match_ports`]), and the impl's body and
//! template ([`plan`]), a wrapper `.subckt` in the cell's pin order when the ports come in
//! another order; the extracted text itself is never edited.
//!
//! Cites: netlist spec §7.1 (default templates, bodies, `${NAME}`), §7.6.6 (`{pins}`, the
//! port order of a leaf row).
//!
//! Contents (`pub(super)`):
//! - `struct Scan { subckts, needs }`, `struct Plan { format, body, wrapper }`
//! - `fn scan(&str) -> Result<Scan, String>`
//! - `fn pick(&Scan, &str) -> Result<(String, usize, u32), String>`
//! - `fn port_bits(&Elab) -> Vec<String>`
//! - `fn match_ports(&[String], &[String]) -> Result<(), String>`
//! - `fn plan(&str, &str, &[String], &[String], &str, &[&str], &str) -> Plan`

use std::collections::BTreeSet;

use schemify_elab::{Elab, Name};

/// What annotate reads from an extracted netlist.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Scan {
    /// `.subckt` definitions: name, byte offset of its line, line number; file order.
    pub(super) subckts: Vec<(String, usize, u32)>,
    /// The device models the text calls (`X` subcircuits, `M` models) and does not define
    /// itself, lowercase (SPICE names compare without case), sorted.
    pub(super) needs: Vec<String>,
}

/// The impl annotate writes.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Plan {
    /// Its `format spice` template; `None`: the default template, whose `{cell}` is the
    /// cell stem (netlist spec §7.1).
    pub(super) format: Option<String>,
    /// Its body: the wrapper, if any, then the extracted text.
    pub(super) body: Vec<String>,
    /// The wrapper's name, when the ports needed one.
    pub(super) wrapper: Option<String>,
}

/// One logical SPICE line: a `+` line joined to the one before; blank lines, `*` comments
/// and inline `;`/`$` comments dropped (as `schemify_elab::impl_ports` reads them).
struct Logical {
    /// Byte offset of its first physical line.
    at: usize,
    /// 1-based number of that line.
    line: u32,
    text: String,
}

/// Reads the `.subckt` definitions and the device calls of an extracted netlist.
///
/// # Errors
///
/// Text that would land outside the bench's DUT, by line: an element outside every
/// `.subckt` (it would join the bench's top level), `.end` (it would end the bench's
/// netlist), a `.subckt` without a name or without its `.ends`, an `.ends` without one.
pub(super) fn scan(text: &str) -> Result<Scan, String> {
    let mut s = Scan::default();
    let (mut defined, mut calls) = (BTreeSet::new(), BTreeSet::new());
    // Line numbers of the `.subckt`s still open, innermost last.
    let mut open: Vec<u32> = Vec::new();
    for l in logical(text) {
        let mut words = l.text.split_whitespace();
        let first = words.next().expect("a logical line has a word");
        let word = first.to_ascii_lowercase();
        match word.as_str() {
            ".subckt" => {
                let name = words
                    .next()
                    .ok_or_else(|| format!("line {}: .subckt without a name", l.line))?;
                defined.insert(name.to_ascii_lowercase());
                s.subckts.push((name.to_owned(), l.at, l.line));
                open.push(l.line);
            }
            ".ends" => {
                open.pop()
                    .ok_or_else(|| format!("line {}: .ends without .subckt", l.line))?;
            }
            ".end" => {
                return Err(format!(
                    "line {}: .end would end the bench's netlist",
                    l.line
                ));
            }
            ".model" => {
                if let Some(m) = words.next() {
                    defined.insert(m.to_ascii_lowercase());
                }
            }
            _ if first.starts_with('.') => {}
            _ if open.is_empty() => {
                return Err(format!(
                    "line {}: `{first}` is outside every .subckt, so it would join the bench's \
                     top level",
                    l.line
                ));
            }
            // ponytail: X and M calls only (sky130 extracts every device as an X call);
            // diode and BJT model names follow when a kit extracts them as D or Q lines.
            w if w.starts_with('x') => {
                if let Some(m) = words.rfind(|w| !w.contains('=')) {
                    calls.insert(m.to_ascii_lowercase());
                }
            }
            w if w.starts_with('m') => {
                if let Some(m) = words.nth(4).filter(|w| !w.contains('=')) {
                    calls.insert(m.to_ascii_lowercase());
                }
            }
            _ => {}
        }
    }
    if let Some(line) = open.last() {
        return Err(format!("line {line}: .subckt without .ends"));
    }
    s.needs = calls.into_iter().filter(|c| !defined.contains(c)).collect();
    assert!(s.needs.windows(2).all(|w| w[0] < w[1]));
    Ok(s)
}

/// The `.subckt` named `want` (ASCII case-insensitively): its name as written, the byte
/// offset of its line and the line number.
///
/// # Errors
///
/// No such `.subckt` (naming those there are), or more than one.
pub(super) fn pick(s: &Scan, want: &str) -> Result<(String, usize, u32), String> {
    let hits: Vec<&(String, usize, u32)> = s
        .subckts
        .iter()
        .filter(|d| d.0.eq_ignore_ascii_case(want))
        .collect();
    match hits.as_slice() {
        [one] => {
            assert!(one.0.eq_ignore_ascii_case(want));
            Ok((*one).clone())
        }
        [] if s.subckts.is_empty() => Err("no .subckt".to_owned()),
        [] => {
            let found: Vec<&str> = s.subckts.iter().map(|d| d.0.as_str()).collect();
            Err(format!(
                "no .subckt {want} (it defines {}; name one with --subckt)",
                found.join(", ")
            ))
        }
        many => {
            let at: Vec<String> = many.iter().map(|d| d.2.to_string()).collect();
            Err(format!(
                ".subckt {want} is defined {} times (lines {})",
                many.len(),
                at.join(", ")
            ))
        }
    }
}

/// The cell's port bits in the order a leaf row connects them (`{pins}`, netlist spec
/// §7.6.6): the ordinary pins' bits as written (an alias pin adds none), then the
/// inherited pins'. `e` elaborates the cell itself, so its interface is `e.cells[0]`.
pub(super) fn port_bits(e: &Elab) -> Vec<String> {
    let f = &e.cells[0];
    let ordinary = (0..f.pin_name.len())
        .filter(|&p| f.pin_inherit[p] == Name::NONE && f.pin_alias[p] == u32::MAX);
    let inherited = (0..f.pin_name.len()).filter(|&p| f.pin_inherit[p] != Name::NONE);
    let bits: Vec<String> = ordinary
        .chain(inherited)
        .flat_map(|p| f.bits(p))
        .map(|b| e.names.get(*b).to_owned())
        .collect();
    assert!(
        bits.len() >= f.bits_total as usize,
        "every ordinary bit is a port"
    );
    bits
}

/// Checks that `pins` and `ports` are one set of names, ASCII case-insensitively (SPICE
/// node names are).
///
/// # Errors
///
/// The pins the ports lack and the ports the cell lacks, by name; or two names either side
/// that differ only in case (SPICE cannot tell them apart).
pub(super) fn match_ports(pins: &[String], ports: &[String]) -> Result<(), String> {
    for (side, names) in [("pins", pins), ("ports", ports)] {
        let mut keys: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
        keys.sort_unstable();
        if let Some(w) = keys.windows(2).find(|w| w[0] == w[1]) {
            return Err(format!("two {side} named {} up to case", w[0]));
        }
    }
    let keys =
        |v: &[String]| -> BTreeSet<String> { v.iter().map(|n| n.to_ascii_lowercase()).collect() };
    let (pin_keys, port_keys) = (keys(pins), keys(ports));
    let missing: Vec<&str> = pins
        .iter()
        .filter(|p| !port_keys.contains(&p.to_ascii_lowercase()))
        .map(String::as_str)
        .collect();
    let extra: Vec<&str> = ports
        .iter()
        .filter(|p| !pin_keys.contains(&p.to_ascii_lowercase()))
        .map(String::as_str)
        .collect();
    if missing.is_empty() && extra.is_empty() {
        assert_eq!(pins.len(), ports.len());
        return Ok(());
    }
    let mut why = Vec::new();
    if !missing.is_empty() {
        why.push(format!("missing {}", missing.join(" ")));
    }
    if !extra.is_empty() {
        why.push(format!("extra {}", extra.join(" ")));
    }
    Err(why.join("; "))
}

/// Plans the impl of cell `stem` for the extracted `text`, whose `.subckt pname` declares
/// `ports` (the names of `pins`, checked by [`match_ports`]). Used as is when its ports
/// come in pin order and it is named after the cell; otherwise a wrapper `.subckt` in the
/// cell's pin order calls it, so the extracted text is never edited. The wrapper is named
/// after the cell, unless the text defines that name (`defined`): then `<stem>__<imp>`,
/// called through a `format spice` line.
pub(super) fn plan(
    stem: &str,
    imp: &str,
    pins: &[String],
    ports: &[String],
    pname: &str,
    defined: &[&str],
    text: &str,
) -> Plan {
    assert_eq!(pins.len(), ports.len(), "matched by match_ports");
    let in_order = pins
        .iter()
        .zip(ports)
        .all(|(a, b)| a.eq_ignore_ascii_case(b));
    let verbatim = text.lines().map(escape);
    if in_order && pname.eq_ignore_ascii_case(stem) {
        return Plan {
            format: None,
            body: verbatim.collect(),
            wrapper: None,
        };
    }
    let taken = |n: &str| defined.iter().any(|d| d.eq_ignore_ascii_case(n));
    let mut name = stem.to_owned();
    if taken(&name) {
        name = format!("{stem}__{imp}");
        // Bounded: each pass skips one name the text defines.
        for _ in 0..=defined.len() {
            if !taken(&name) {
                break;
            }
            name.push('_');
        }
    }
    assert!(!taken(&name));
    let pin_of = |port: &String| -> &str {
        pins.iter()
            .find(|p| p.eq_ignore_ascii_case(port))
            .expect("matched by match_ports")
    };
    let call: Vec<&str> = ports.iter().map(pin_of).collect();
    let mut body = vec![
        format!("* schemify annotate: {stem}'s pins in pin order, onto .subckt {pname}"),
        format!(".subckt {name} {}", pins.join(" ")),
        format!("X1 {} {pname}", call.join(" ")),
        format!(".ends {name}"),
    ];
    body.extend(verbatim);
    Plan {
        format: (name != stem).then(|| format!("X{{name}} {{pins}} {name}")),
        body,
        wrapper: Some(name),
    }
}

// ---- private ----

/// The logical lines of `text`, in file order.
fn logical(text: &str) -> Vec<Logical> {
    let mut out: Vec<Logical> = Vec::new();
    let mut at = 0;
    for (k, raw) in text.split_inclusive('\n').enumerate() {
        let start = at;
        at += raw.len();
        let line = raw.trim_start();
        let line = line[..line.find([';', '$']).unwrap_or(line.len())].trim_end();
        if line.is_empty() || line.starts_with('*') {
            continue;
        }
        match (line.strip_prefix('+'), out.last_mut()) {
            (Some(rest), Some(prev)) => {
                prev.text.push(' ');
                prev.text.push_str(rest);
            }
            _ => out.push(Logical {
                at: start,
                line: k as u32 + 1,
                text: line.to_owned(),
            }),
        }
    }
    assert_eq!(at, text.len());
    assert!(out.windows(2).all(|w| w[0].at < w[1].at));
    out
}

/// `line` with each `$` that `${NAME}` expansion would read (one before `$` or `{`, netlist
/// spec §7.1) doubled, so the netlist carries the line as the extractor wrote it.
fn escape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '$' && matches!(chars.peek(), Some('$' | '{')) {
            out.push('$');
        }
    }
    assert!(out.len() >= line.len());
    out
}

#[cfg(test)]
mod tests {
    use super::{Scan, escape, match_ports, pick, plan, scan};

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn scan_reads_subckts_and_calls_and_refuses_what_would_leak() {
        let text = "* header\n.subckt inner a b\nR1 a b 1k\n.ends\n\n.SUBCKT Top x\n+ y\n\
                    X0 x y inner\nX1 x y sky130_fd_pr__nfet_01v8 w=1 l=0.15\nM2 x y 0 0 nch w=1\n\
                    C1 x 0 1f $ **FLOATING\n.ends Top\n";
        let s = scan(text).unwrap();
        let names: Vec<(&str, u32)> = s.subckts.iter().map(|d| (d.0.as_str(), d.2)).collect();
        assert_eq!(names, [("inner", 2), ("Top", 6)]);
        assert_eq!(&text[s.subckts[1].1..s.subckts[1].1 + 7], ".SUBCKT");
        // `inner` is defined in the text, so only the kit's devices are needed.
        assert_eq!(s.needs, ["nch", "sky130_fd_pr__nfet_01v8"]);
        let cases = [
            ("R1 a b 1k\n", "line 1: `R1` is outside every .subckt"),
            (".subckt a x\n.ends\n.end\n", "line 3: .end would end"),
            (".subckt a x\nR1 x 0 1\n", "line 1: .subckt without .ends"),
            (".ends\n", "line 1: .ends without .subckt"),
            ("\n.subckt\n", "line 2: .subckt without a name"),
        ];
        for (text, why) in cases {
            let e = scan(text).expect_err(text);
            assert!(e.starts_with(why), "{text}: {e}");
        }
    }

    #[test]
    fn pick_names_what_is_there() {
        let s = scan(".subckt dp_layout a\n.ends\n.subckt core a\n.ends\n").unwrap();
        assert_eq!(pick(&s, "DP_Layout").unwrap().0, "dp_layout");
        let e = pick(&s, "dp").unwrap_err();
        assert!(
            e.contains("no .subckt dp") && e.contains("dp_layout, core"),
            "{e}"
        );
        let twice = scan(".subckt a x\n.ends\n.subckt A x\n.ends\n").unwrap();
        assert!(
            pick(&twice, "a")
                .unwrap_err()
                .contains("2 times (lines 1, 3)")
        );
        assert_eq!(pick(&Scan::default(), "a").unwrap_err(), "no .subckt");
    }

    #[test]
    fn ports_match_pins_by_name_up_to_case() {
        let pins = strings(&["inp", "inn", "out", "vss"]);
        assert!(match_ports(&pins, &strings(&["OUT", "inn", "inp", "VSS"])).is_ok());
        let e = match_ports(&pins, &strings(&["inp", "inn", "out", "VSUBS"])).unwrap_err();
        assert_eq!(e, "missing vss; extra VSUBS");
        let e = match_ports(&pins, &strings(&["inp", "inn", "out"])).unwrap_err();
        assert_eq!(e, "missing vss");
        let e = match_ports(&strings(&["a", "A"]), &strings(&["a", "b"])).unwrap_err();
        assert!(e.starts_with("two pins named a"), "{e}");
        let e = match_ports(&strings(&["a", "b"]), &strings(&["B", "b"])).unwrap_err();
        assert!(e.starts_with("two ports named b"), "{e}");
    }

    #[test]
    fn plan_uses_as_is_or_wraps_without_editing_the_text() {
        let pins = strings(&["a", "b"]);
        let text = ".subckt dp a b\nR1 a b 1k\n.ends dp\n";
        let p = plan(
            "dp",
            "pex",
            &pins,
            &strings(&["A", "B"]),
            "DP",
            &["DP"],
            text,
        );
        assert_eq!((p.format, p.wrapper), (None, None));
        assert_eq!(p.body, text.lines().collect::<Vec<_>>());
        // Reordered and named after the cell: the wrapper takes another name and a template.
        let swapped = ".subckt dp b a\nR1 a b 1k\n.ends dp\n";
        let p = plan(
            "dp",
            "pex",
            &pins,
            &strings(&["b", "a"]),
            "dp",
            &["dp", "dp__pex"],
            swapped,
        );
        assert_eq!(p.wrapper.as_deref(), Some("dp__pex_"));
        assert_eq!(p.format.as_deref(), Some("X{name} {pins} dp__pex_"));
        assert_eq!(
            p.body[1..4],
            [".subckt dp__pex_ a b", "X1 b a dp", ".ends dp__pex_"]
        );
        assert_eq!(p.body[4..], swapped.lines().collect::<Vec<_>>());
        // Another name in pin order: a wrapper named after the cell, the default template.
        let p = plan(
            "dp",
            "pex",
            &pins,
            &pins,
            "core",
            &["core"],
            ".subckt core a b\n.ends\n",
        );
        assert_eq!((p.wrapper.as_deref(), p.format), (Some("dp"), None));
        assert_eq!(p.body[1..4], [".subckt dp a b", "X1 a b core", ".ends dp"]);
    }

    #[test]
    fn escape_keeps_the_text_through_name_expansion() {
        for (line, want) in [
            ("C1 a b 1f $ **FLOATING", "C1 a b 1f $ **FLOATING"),
            // `$$` reads as `$`, and a `$` before neither `$` nor `{` as itself.
            ("* ${HOME} $$", "* $${HOME} $$$"),
            ("$", "$"),
            ("$$$", "$$$$$"),
            ("plain", "plain"),
        ] {
            assert_eq!(escape(line), want, "{line}");
        }
    }
}
