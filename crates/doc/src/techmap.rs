//! Technology classes (pdk spec §5, §6): the `tech.map` a manifest's `lib tech map <file>`
//! names, and the derived `tech` cells that make `tech:nmos` resolve.
//!
//! A derived cell is one `.shprim` interface per (class, flavor) row of the map, built from
//! two indexed interfaces: the class's core symbol (`core:nmos4`) gives the drawing and the
//! pin positions, the PDK cell the map names (`sky130_fd_pr:nfet_01v8`) gives the pin names,
//! the params and the impls. The PDK params the map assigns become `computed` params whose
//! expression is the map's (over the class params, child scope), and the class params
//! (`w l nf m flavor`) are added, so elaboration and netlisting need nothing new: an
//! instance overrides class params and the PDK's format renders the mapped values.
//!
//! Six questions: in, the map bytes and the indexed interfaces; out, derived interface
//! documents. Rows ≤ [`CLASSES_MAX`] (a handful in practice). Rebuilt whole after every
//! library (re)index, because any library may hold a referenced cell. Lifetime: the index.

use crate::document::{Document, Loc, TableId};
use crate::record::RecordBuf;
use schemify_format::{Document as FormatDoc, FileKind, RecordKind};

/// Technology classes the core resolves, with the core symbol each one draws as.
// ponytail: MOS only (pdk spec §10 MVP); res, cap, diode, npn, pnp are one row each here
// plus their class params once their core symbols carry them.
pub const CLASSES: [(&str, &str); 2] = [("nmos", "nmos4"), ("pmos", "pmos4")];
/// Flavors a map row may name; `core` is what `tech:<class>` means without a `flavor`.
pub const FLAVORS: [&str; 4] = ["core", "io", "lvt", "hvt"];
/// Class params of a MOS class (pdk spec §5), in the order they are appended.
pub const CLASS_PARAMS: [&str; 5] = ["w", "l", "nf", "m", "flavor"];
/// Most `class` rows one map holds.
pub const CLASSES_MAX: usize = 1 << 8;

/// A parsed `tech.map` (SoA over its `class` records, params as CSR).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TechMap {
    /// Header `pdk <family>`: the name messages use (`sky130 has no lvt pmos`).
    pub pdk: String,
    /// Header `vdd <volts>`: nominal core supply, as written.
    pub vdd: Option<String>,
    /// Class of each row (`nmos`).
    pub class: Vec<String>,
    /// Flavor of each row (`core`).
    pub flavor: Vec<String>,
    /// The PDK cell each row becomes, a `lib:cell` reference.
    pub cell: Vec<String>,
    /// CSR offsets (`rows + 1`) into the param columns.
    pub param_start: Vec<u32>,
    /// PDK param a row assigns.
    pub param_name: Vec<String>,
    /// Its expression over the class params (the text an instance override would hold).
    pub param_expr: Vec<String>,
}

impl TechMap {
    /// Parses `tech.map` bytes.
    ///
    /// # Errors
    ///
    /// The format error, a file of another kind, more than [`CLASSES_MAX`] rows, or a
    /// flavor outside [`FLAVORS`], as text for the checks panel.
    pub fn parse(bytes: &[u8]) -> Result<TechMap, String> {
        let d = schemify_format::parse(bytes).map_err(|e| e.to_string())?;
        if d.kind() != FileKind::Techmap {
            return Err(format!("not a tech map (`{}` file)", d.kind().word()));
        }
        let mut m = TechMap {
            param_start: vec![0],
            ..TechMap::default()
        };
        let header_attr = |key: &str| d.find_attr(0, key).and_then(|a| d.attr_value(a, 0));
        m.pdk = header_attr("pdk").unwrap_or("this technology").to_owned();
        m.vdd = header_attr("vdd").map(str::to_owned);
        for r in 1..d.record_count() {
            let f: Vec<&str> = d.fields(r).collect();
            assert_eq!(f.len(), 2, "the reader checked the class shape");
            if !FLAVORS.contains(&f[1]) {
                return Err(format!(
                    "class {} {}: flavor must be one of {}",
                    f[0],
                    f[1],
                    FLAVORS.join(" ")
                ));
            }
            if m.class.len() >= CLASSES_MAX {
                return Err(format!("more than {CLASSES_MAX} class rows"));
            }
            let cell = d
                .find_attr(r, "cell")
                .and_then(|a| d.attr_value(a, 0))
                .expect("the reader checked the required `cell`");
            m.class.push(f[0].to_owned());
            m.flavor.push(f[1].to_owned());
            m.cell.push(cell.to_owned());
            for a in d.attrs(r) {
                if d.attr_key(a) == "param" {
                    let v: Vec<&str> = d.attr_values(a).collect();
                    m.param_name.push(v[0].to_owned());
                    m.param_expr.push(v[1].to_owned());
                }
            }
            m.param_start.push(m.param_name.len() as u32);
        }
        assert_eq!(m.param_start.len(), m.class.len() + 1);
        assert_eq!(m.cell.len(), m.flavor.len());
        Ok(m)
    }

    /// Returns the row of (`class`, `flavor`).
    pub fn find(&self, class: &str, flavor: &str) -> Option<usize> {
        let row =
            (0..self.class.len()).find(|&r| self.class[r] == class && self.flavor[r] == flavor);
        assert!(row.is_none_or(|r| r < self.cell.len()));
        row
    }

    /// The `(param, expression)` pairs of row `row`.
    pub fn params(&self, row: usize) -> impl Iterator<Item = (&str, &str)> {
        let (a, b) = (
            self.param_start[row] as usize,
            self.param_start[row + 1] as usize,
        );
        assert!(a <= b && b <= self.param_name.len());
        (a..b).map(|k| (self.param_name[k].as_str(), self.param_expr[k].as_str()))
    }
}

/// Returns the core symbol a class draws as (`nmos` → `nmos4`).
pub fn core_symbol(class: &str) -> Option<&'static str> {
    CLASSES.iter().find(|c| c.0 == class).map(|c| c.1)
}

/// Builds the derived interface of map row `row`: `core` is the class's core symbol,
/// `pdk` the interface of the PDK cell the row names.
///
/// # Errors
///
/// Why the row cannot become a cell, as the checks panel shows it: a PDK pin with no class
/// pin of the same name (ignoring case) or the reverse, a mapped param the PDK cell lacks,
/// or a class param the PDK cell also has that the map assigns other than to itself.
pub(crate) fn derive(
    map: &TechMap,
    row: usize,
    core: &Document,
    pdk: &Document,
) -> Result<Document, String> {
    let (class, flavor, cell) = (&map.class[row], &map.flavor[row], &map.cell[row]);
    let mut d = FormatDoc::new(FileKind::Shprim);
    for a in pdk.header_record().attrs.iter().filter(|a| a.key != "doc") {
        d.push_attr(&a.key);
        a.values.iter().for_each(|v| d.push_value(v));
    }
    d.push_attr("doc");
    d.push_value(&format!("tech:{class} ({flavor}) of {}: {cell}", map.pdk));
    let rows = |doc: &Document, t: TableId| -> Vec<RecordBuf> {
        (0..doc.table_len(t))
            .map(|r| {
                doc.record(Loc {
                    table: t,
                    row: r as u32,
                })
            })
            .collect()
    };
    // Pins: the PDK's names and order (its format names them), the core symbol's positions.
    let core_pins = rows(core, TableId::Pins);
    let mut matched = 0;
    for mut p in rows(pdk, TableId::Pins) {
        let c = core_pins
            .iter()
            .find(|c| c.fields[0].eq_ignore_ascii_case(&p.fields[0]))
            .ok_or_else(|| format!("{cell} pin `{}` has no `{class}` class pin", p.fields[0]))?;
        p.fields[2].clone_from(&c.fields[2]);
        p.fields[3].clone_from(&c.fields[3]);
        matched += 1;
        push(&mut d, &p);
    }
    if matched != core_pins.len() {
        return Err(format!(
            "{cell} lacks a pin of the `{class}` class ({} pins expected)",
            core_pins.len()
        ));
    }
    // Params: the PDK's, mapped ones computed from the class params; then the class params
    // the PDK cell lacks.
    let mapped: Vec<(&str, &str)> = map.params(row).collect();
    let core_params = rows(core, TableId::Params);
    let class_default = |name: &str| -> RecordBuf {
        core_params
            .iter()
            .find(|p| p.fields[0] == name)
            .cloned()
            .unwrap_or_else(|| match name {
                "flavor" => RecordBuf::new(RecordKind::Param)
                    .field("flavor")
                    .field(flavor)
                    .attr("kind", &["choice"])
                    .attr("choices", &FLAVORS)
                    .attr("doc", &["technology flavor (pdk spec §5)"]),
                _ => RecordBuf::new(RecordKind::Param)
                    .field(name)
                    .field("1")
                    .attr("doc", &["fingers"]),
            })
    };
    let pdk_params = rows(pdk, TableId::Params);
    for (name, _) in &mapped {
        if !pdk_params.iter().any(|p| p.fields[0] == *name) {
            return Err(format!("map param `{name}` is not a param of {cell}"));
        }
    }
    for mut p in pdk_params.iter().cloned() {
        let name = p.fields[0].clone();
        let expr = mapped.iter().find(|m| m.0 == name).map(|m| m.1);
        if CLASS_PARAMS.contains(&name.as_str()) {
            if expr.is_some_and(|e| e.trim() != name) {
                return Err(format!(
                    "{cell} param `{name}` is a class param: map it as `param {name} {name}` or not at all"
                ));
            }
            // The class param stands in for the PDK's: its default, the PDK's attributes.
            p.fields[1].clone_from(&class_default(&name).fields[1]);
        } else if let Some(e) = expr {
            p.fields[1] = e.to_owned();
            p.attrs.retain(|a| a.key != "computed");
            p.attrs.push(crate::record::Attr {
                key: "computed".to_owned(),
                values: vec![e.to_owned()],
            });
        }
        push(&mut d, &p);
    }
    for name in CLASS_PARAMS {
        if !pdk_params.iter().any(|p| p.fields[0] == name) {
            push(&mut d, &class_default(name));
        }
    }
    for i in rows(pdk, TableId::Impls) {
        push(&mut d, &i);
    }
    for g in rows(core, TableId::Graphics) {
        push(&mut d, &g);
    }
    // Round trip through the canonical text: the reader validates what was assembled.
    let text = schemify_format::write(&d);
    let parsed = schemify_format::parse(text.as_bytes())
        .map_err(|e| format!("derived cell does not parse: {e}"))?;
    let doc = Document::from_format(&parsed).doc;
    assert_eq!(doc.kind(), FileKind::Shprim);
    assert_eq!(doc.pins().name.len(), core_pins.len());
    Ok(doc)
}

/// Appends `rec` to `d` (comments dropped: a derived cell is never written to disk).
fn push(d: &mut FormatDoc<'static>, rec: &RecordBuf) {
    d.begin_record(rec.kind);
    rec.fields.iter().for_each(|f| d.push_field(f));
    for a in &rec.attrs {
        d.push_attr(&a.key);
        a.values.iter().for_each(|v| d.push_value(v));
    }
    rec.verbatim.iter().for_each(|v| d.push_verbatim(v));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_rows_and_params() {
        let m = TechMap::parse(
            b"schemify 1 techmap\n  pdk sky130\n  vdd 1.8\n\nclass nmos core\n  cell s:n\n  param W w/1u\n  param L l/1u\nclass pmos core\n  cell s:p\n",
        )
        .expect("parses");
        assert_eq!((m.pdk.as_str(), m.vdd.as_deref()), ("sky130", Some("1.8")));
        assert_eq!(m.find("pmos", "core"), Some(1));
        assert_eq!(m.find("pmos", "lvt"), None);
        assert_eq!(
            m.params(0).collect::<Vec<_>>(),
            [("W", "w/1u"), ("L", "l/1u")]
        );
        assert_eq!(m.params(1).count(), 0);
        let bad = TechMap::parse(b"schemify 1 techmap\nclass nmos svt\n  cell s:n\n");
        assert!(bad.unwrap_err().contains("flavor"));
        assert!(
            TechMap::parse(b"schemify 1 lock\n")
                .unwrap_err()
                .contains("not a tech map")
        );
        assert_eq!(core_symbol("pmos"), Some("pmos4"));
        assert_eq!(core_symbol("res"), None);
    }
}
