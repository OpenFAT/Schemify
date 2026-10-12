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

/// Pins in netlist order (SoA). Arena for `name`: [`SymbolInterface::names`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pins {
    /// `name=` value (empty when missing).
    pub name: Vec<Span>,
    /// `dir=` value.
    pub dir: Vec<PinDir>,
    /// Box centre in milli-units, rounded half away from zero (D33).
    pub center: Vec<[i32; 2]>,
}

/// The netlisting interface of one symbol.
#[derive(Clone, Debug, PartialEq)]
pub struct SymbolInterface {
    /// Symbol attributes, tokenized: the last `K` if non-empty, else the first
    /// non-empty `G`, else empty.
    pub attrs: Attrs,
    /// The `template` value, tokenized (empty when absent).
    pub template: Attrs,
    /// Pins in netlist order.
    pub pins: Pins,
    /// Arena for pin names.
    pub names: String,
}

impl SymbolInterface {
    /// Returns pin `i`'s name (netlist order).
    ///
    /// # Panics
    ///
    /// Panics if `i` is not a pin row.
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
/// use interop::{symbol::extract, xschem::read};
/// let f = read("K {type=res template=\"name=R1 spiceprefix=X\"}\n\
///               B 5 -2.5 27.5 2.5 32.5 {name=M dir=inout sim_pinnumber=2}\n\
///               B 5 -2.5 -32.5 2.5 -27.5 {name=P dir=inout sim_pinnumber=1}\n").unwrap();
/// let s = extract(&f);
/// assert_eq!((s.pin_name(0), s.pins.center[0]), ("P", [0, -30_000]));
/// assert_eq!(s.template.get("spiceprefix"), Some("X"));
/// ```
pub fn extract(file: &XschemFile) -> SymbolInterface {
    let g = &file.globals;
    let non_empty = |i: &usize| !file.str(g.text[*i]).trim().is_empty();
    let last_k = (0..g.tag.len())
        .rev()
        .find(|&i| g.tag[i] == GlobalTag::K)
        .filter(non_empty);
    let first_g = (0..g.tag.len()).find(|i| g.tag[*i] == GlobalTag::G && non_empty(i));
    let text = last_k.or(first_g).map_or("", |i| file.str(g.text[i]));
    let attrs = Attrs::parse(text);
    let template = Attrs::parse(attrs.get("template").unwrap_or(""));

    let mut pins = Pins::default();
    let mut sim_pinnumber: Vec<Option<u32>> = Vec::new();
    let mut names = String::new();
    let mut props = Attrs::default();
    for row in 0..file.boxes.layer.len() {
        if file.boxes.layer[row] != PIN_LAYER {
            continue;
        }
        props.parse_into(file.str(file.boxes.props[row]));
        let [x1, y1, x2, y2] = file.boxes.xy[row].map(i64::from);
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
        pins.center.push([half(x1 + x2), half(y1 + y2)]);
        sim_pinnumber.push(
            props
                .get("sim_pinnumber")
                .and_then(|v| v.trim().parse::<u32>().ok()),
        );
    }
    let n = pins.name.len();
    if n > 0 && sim_pinnumber.iter().all(Option::is_some) {
        let mut perm: Vec<usize> = (0..n).collect();
        perm.sort_by_key(|&i| sim_pinnumber[i]); // stable: ties keep file order
        pins = Pins {
            name: perm.iter().map(|&i| pins.name[i]).collect(),
            dir: perm.iter().map(|&i| pins.dir[i]).collect(),
            center: perm.iter().map(|&i| pins.center[i]).collect(),
        };
    }
    assert!(pins.name.len() == n && pins.dir.len() == n && pins.center.len() == n);
    assert!(n <= file.boxes.layer.len());
    SymbolInterface {
        attrs,
        template,
        pins,
        names,
    }
}

/// Halves a sum of two `i32`s, rounding half away from zero.
fn half(sum: i64) -> i32 {
    let q = sum / 2 + if sum % 2 == 0 { 0 } else { sum.signum() };
    // Two i32s average to an i32, and rounding away from zero cannot leave that range.
    i32::try_from(q).expect("mean of two i32 fits i32")
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
        assert_eq!(
            (0..3).map(|i| s.pin_name(i)).collect::<Vec<_>>(),
            ["M", "P", "B"]
        );
        assert_eq!(s.template.get("L"), Some("0.35"));
        // One pin without sim_pinnumber keeps file order.
        let s =
            extract(&read("B 5 0 0 1 1 {name=a sim_pinnumber=2}\nB 5 0 0 1 1 {name=b}\n").unwrap());
        assert_eq!(s.pin_name(0), "a");
    }

    #[test]
    fn k_over_g_and_pin_details() {
        let src = "G {type=legacy}\nK {}\nB 5 19.921875 -0.078125 20.078125 0.078125 {name=B dir=in}\nB 4 0 0 1 1 {name=notapin}\nB 5 0 0 0.001 0 {name=C dir=weird sim_pinnumber=x}\n";
        let s = extract(&read(src).unwrap());
        assert_eq!(s.attrs.get("type"), Some("legacy"));
        assert_eq!(s.pins.center[0], [20_000, 0]);
        assert_eq!(s.pins.dir, [PinDir::In, PinDir::Other]);
        assert_eq!(s.pins.center[1], [1, 0]); // half a milli rounds away from zero
        let s = extract(&read("G {type=a}\nK {type=b}\n").unwrap());
        assert_eq!(s.attrs.get("type"), Some("b"));
        assert!(extract(&read("").unwrap()).attrs.is_empty());
    }
}
