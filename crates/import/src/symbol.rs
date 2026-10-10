//! Symbol interface of an xschem `.sym` (research: `pdk-xschem-devices.md` §1, §5.2;
//! `xschem-feature-inventory.md` §5.8, §5.12): the attribute block, its `template`
//! defaults, and the pins in netlist order.
//!
//! Rules taken from the research:
//! - Symbol attributes come from the last `K` record if it is non-empty, else from the
//!   first non-empty `G` (pre-1.2 files).
//! - Pins are the layer-5 `B` records. Netlist order is file order, unless **every** pin
//!   carries an integer `sim_pinnumber`, in which case pins are sorted by it (stable, so
//!   equal numbers keep file order). `pinnumber` never affects order.
//! - A pin's position is the centre of its box.
//!
//! A schematic's ports (its `ipin`/`opin`/`iopin` instances) need symbol resolution and
//! are the translator's job, not this module's.

use crate::Span;
use crate::attr::Attrs;
use crate::xschem::{GlobalTag, XschemFile};

/// The layer xschem draws symbol pins on.
pub const PIN_LAYER: u8 = 5;

/// Where the symbol attributes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttrSource {
    /// A non-empty `K` record (file format ≥ 1.2).
    K,
    /// A non-empty legacy `G` record, used because no non-empty `K` exists.
    G,
    /// Neither: the symbol has no attributes.
    None,
}

/// How the pin table is ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinOrder {
    /// File order of the `B 5` records.
    File,
    /// Ascending `sim_pinnumber` (every pin had one).
    SimPinnumber,
}

/// A pin's `dir=` attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinDir {
    /// `dir=in`.
    In,
    /// `dir=out`.
    Out,
    /// `dir=inout`.
    InOut,
    /// No `dir=`.
    Missing,
    /// Any other value (kept raw in the box's props).
    Other,
}

/// Attributes the importer routes by name; [`SymbolInterface::get`] looks them up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymKey {
    /// `type`: symbol class (`nmos`, `subcircuit`, `label`, …).
    Type,
    /// `format`: SPICE netlist line template.
    Format,
    /// `lvs_format`: LVS netlist line template.
    LvsFormat,
    /// `template`: default instance attributes (also parsed into [`SymbolInterface::template`]).
    Template,
    /// `spice_ignore`: `true`/`open`/`short`/`false`.
    SpiceIgnore,
    /// `schematic`: alternate schematic for the symbol.
    Schematic,
    /// `default_schematic`: `ignore` disables the implicit schematic.
    DefaultSchematic,
    /// `device_model`: text emitted once per netlist.
    DeviceModel,
    /// `spiceprefix`, looked up in the `template` (it is an instance default).
    SpicePrefix,
}

/// Pins in netlist order (SoA). Arena for `name`: [`SymbolInterface::names`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pins {
    /// Row of the pin's box in the source [`XschemFile::boxes`].
    pub box_row: Vec<u32>,
    /// `name=` value (empty when missing).
    pub name: Vec<Span>,
    /// `dir=` value.
    pub dir: Vec<PinDir>,
    /// Box centre in milli-units.
    pub center: Vec<[i32; 2]>,
    /// `sim_pinnumber=` as an integer, when present and numeric.
    pub sim_pinnumber: Vec<Option<u32>>,
}

/// The netlisting interface of one symbol.
#[derive(Clone, Debug, PartialEq)]
pub struct SymbolInterface {
    /// Symbol attributes, tokenized.
    pub attrs: Attrs,
    /// Which record [`SymbolInterface::attrs`] came from.
    pub source: AttrSource,
    /// The `template` value, tokenized (empty when absent).
    pub template: Attrs,
    /// Pins in netlist order.
    pub pins: Pins,
    /// How [`SymbolInterface::pins`] is ordered.
    pub order: PinOrder,
    /// Arena for pin names.
    pub names: String,
    /// Pins whose box centre was half a milli-unit off-grid and got rounded (D33).
    pub rounded_centers: Vec<u32>,
}

impl SymbolInterface {
    /// Returns the value of `key`, or `None` when the symbol does not set it.
    pub fn get(&self, key: SymKey) -> Option<&str> {
        let name = match key {
            SymKey::Type => "type",
            SymKey::Format => "format",
            SymKey::LvsFormat => "lvs_format",
            SymKey::Template => "template",
            SymKey::SpiceIgnore => "spice_ignore",
            SymKey::Schematic => "schematic",
            SymKey::DefaultSchematic => "default_schematic",
            SymKey::DeviceModel => "device_model",
            SymKey::SpicePrefix => return self.template.get("spiceprefix"),
        };
        self.attrs.get(name)
    }

    /// Returns pin `i`'s name (netlist order).
    pub fn pin_name(&self, i: usize) -> &str {
        self.pins.name[i].of(&self.names)
    }
}

/// Extracts the symbol interface of a top-level file (embedded bodies are files too).
///
/// # Time complexity
///
/// O(attribute bytes + pins · log pins).
///
/// # Examples
///
/// ```
/// use schemify_import::{symbol::{extract, PinOrder, SymKey}, xschem::read};
/// let f = read("K {type=res template=\"name=R1 spiceprefix=X\"}\n\
///               B 5 -2.5 27.5 2.5 32.5 {name=M dir=inout sim_pinnumber=2}\n\
///               B 5 -2.5 -32.5 2.5 -27.5 {name=P dir=inout sim_pinnumber=1}\n").unwrap();
/// let s = extract(&f);
/// assert_eq!(s.order, PinOrder::SimPinnumber);
/// assert_eq!((s.pin_name(0), s.pins.center[0]), ("P", [0, -30_000]));
/// assert_eq!(s.get(SymKey::SpicePrefix), Some("X"));
/// ```
pub fn extract(file: &XschemFile) -> SymbolInterface {
    let g = &file.globals;
    let last_k = (0..g.tag.len()).rev().find(|&i| g.tag[i] == GlobalTag::K);
    let first_g = (0..g.tag.len())
        .find(|&i| g.tag[i] == GlobalTag::G && !file.str(g.text[i]).trim().is_empty());
    let (source, text) = match (
        last_k.filter(|&i| !file.str(g.text[i]).trim().is_empty()),
        first_g,
    ) {
        (Some(k), _) => (AttrSource::K, file.str(g.text[k])),
        (None, Some(gi)) => (AttrSource::G, file.str(g.text[gi])),
        (None, None) => (AttrSource::None, ""),
    };
    let attrs = Attrs::parse(text);
    let template = Attrs::parse(attrs.get("template").unwrap_or(""));

    let mut pins = Pins::default();
    let mut names = String::new();
    let mut rounded_centers = Vec::new();
    let mut props = Attrs::default();
    for row in 0..file.boxes.layer.len() {
        if file.boxes.layer[row] != PIN_LAYER {
            continue;
        }
        props.parse_into(file.str(file.boxes.props[row]));
        let [x1, y1, x2, y2] = file.boxes.xy[row].map(i64::from);
        let (cx, rx) = half(x1 + x2);
        let (cy, ry) = half(y1 + y2);
        if rx || ry {
            rounded_centers.push(pins.box_row.len() as u32);
        }
        pins.box_row.push(row as u32);
        pins.name.push(crate::push_span(
            &mut names,
            props.get("name").unwrap_or(""),
        ));
        pins.dir.push(match props.get("dir") {
            None => PinDir::Missing,
            Some("in") => PinDir::In,
            Some("out") => PinDir::Out,
            Some("inout") => PinDir::InOut,
            Some(_) => PinDir::Other,
        });
        pins.center.push([cx, cy]);
        pins.sim_pinnumber.push(
            props
                .get("sim_pinnumber")
                .and_then(|v| v.trim().parse::<u32>().ok()),
        );
    }
    let n = pins.box_row.len();
    let order = if n > 0 && pins.sim_pinnumber.iter().all(Option::is_some) {
        let mut perm: Vec<usize> = (0..n).collect();
        perm.sort_by_key(|&i| pins.sim_pinnumber[i]); // stable: ties keep file order
        let mut inverse = vec![0u32; n];
        for (new, &old) in perm.iter().enumerate() {
            inverse[old] = new as u32;
        }
        for r in &mut rounded_centers {
            *r = inverse[*r as usize];
        }
        rounded_centers.sort_unstable();
        pins = Pins {
            box_row: perm.iter().map(|&i| pins.box_row[i]).collect(),
            name: perm.iter().map(|&i| pins.name[i]).collect(),
            dir: perm.iter().map(|&i| pins.dir[i]).collect(),
            center: perm.iter().map(|&i| pins.center[i]).collect(),
            sim_pinnumber: perm.iter().map(|&i| pins.sim_pinnumber[i]).collect(),
        };
        PinOrder::SimPinnumber
    } else {
        PinOrder::File
    };
    assert!(pins.name.len() == n && pins.dir.len() == n && pins.center.len() == n);
    assert!(order == PinOrder::File || pins.sim_pinnumber.windows(2).all(|w| w[0] <= w[1]));
    SymbolInterface {
        attrs,
        source,
        template,
        pins,
        order,
        names,
        rounded_centers,
    }
}

/// Halves a sum of two `i32`s, rounding half away from zero; reports whether it rounded.
fn half(sum: i64) -> (i32, bool) {
    let q = sum / 2 + if sum % 2 == 0 { 0 } else { sum.signum() };
    // Two i32s average to an i32, and rounding away from zero cannot leave that range.
    (
        i32::try_from(q).expect("mean of two i32 fits i32"),
        sum % 2 != 0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xschem::read;

    #[test]
    fn file_order_unless_every_pin_has_sim_pinnumber() {
        // res_high_po_0p35.sym (PDK doc §2.1): pinnumber never reorders → M P B.
        let src = "K {type=high_precision_poly_resistor\ntemplate=\"name=R1\nL=0.35\nmodel=res_high_po_0p35\nspiceprefix=X\nmult=1\"}\n\
                   B 5 -2.5 27.5 2.5 32.5 {name=M dir=inout propag=1 pinnumber=2}\n\
                   B 5 -2.5 -32.5 2.5 -27.5 {name=P dir=inout propag=0 pinnumber=1}\n\
                   B 5 -22.5 -2.5 -17.5 2.5 {name=B dir=inout pinnumber=1}\n";
        let s = extract(&read(src).unwrap());
        assert_eq!(s.order, PinOrder::File);
        assert_eq!(
            (0..3).map(|i| s.pin_name(i)).collect::<Vec<_>>(),
            ["M", "P", "B"]
        );
        assert_eq!(s.template.get("L"), Some("0.35"));
        // One pin without sim_pinnumber keeps file order.
        let s =
            extract(&read("B 5 0 0 1 1 {name=a sim_pinnumber=2}\nB 5 0 0 1 1 {name=b}\n").unwrap());
        assert_eq!((s.order, s.pin_name(0)), (PinOrder::File, "a"));
    }

    #[test]
    fn k_over_g_and_pin_details() {
        let src = "G {type=legacy}\nK {}\nB 5 19.921875 -0.078125 20.078125 0.078125 {name=B dir=in}\nB 4 0 0 1 1 {name=notapin}\nB 5 0 0 0.001 0 {name=C dir=weird sim_pinnumber=x}\n";
        let s = extract(&read(src).unwrap());
        assert_eq!(
            (s.source, s.get(SymKey::Type)),
            (AttrSource::G, Some("legacy"))
        );
        assert_eq!(s.pins.center[0], [20_000, 0]);
        assert_eq!(s.pins.dir, [PinDir::In, PinDir::Other]);
        assert_eq!(s.rounded_centers, [1]);
        let s = extract(&read("G {type=a}\nK {type=b}\n").unwrap());
        assert_eq!((s.source, s.get(SymKey::Type)), (AttrSource::K, Some("b")));
        assert_eq!(extract(&read("").unwrap()).source, AttrSource::None);
    }
}
