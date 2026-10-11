//! The import report (spec `import.md` §7): per-file record counts, every item that did
//! not translate one-to-one, and the count-conservation invariant
//! `records in = records out + records reported`.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::template::Reason;

/// Why a construct was dropped. Dropped constructs have no netlist effect in xschem
/// (graphics, display text, launchers) or are reported elsewhere; anything that could
/// change a netlist is a [`Class::Review`] instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Drop {
    /// A record the xschem reader itself dropped (malformed).
    Malformed,
    /// A symbol that is not a cell (label, launcher, title, code, probe, …): its
    /// meaning is carried by the records its instances become.
    NotACell,
    /// An xschem `devices/` symbol replaced by a bundled `core` cell.
    MappedToCore,
    /// Schematic drawings (`L B P A` in a `.sch`) and graphs: not part of v1 rank 3.
    Graphic,
    /// Display-only text (OP annotation Tcl, `@spice_get_*`, floaters, hidden text).
    Display,
    /// A zero-length wire.
    ZeroWire,
    /// An instance of a non-electrical symbol (launcher, logo, title, display probe).
    NonElectrical,
    /// VHDL/Verilog/tEDAx global text (`G`/`V`/`E` blocks).
    OtherNetlister,
    /// `N`/`C` records inside a `.sym` (xschem draws them; no connectivity).
    SymbolNetlistRecord,
    /// A graphic on a layer xschem does not load by default (≥ 22).
    Layer,
    /// An embedded symbol (`[ … ]`): the library symbol of the same name is used.
    Embedded,
    /// `only_toplevel=true` content of a component schematic (xschem netlists it only
    /// when the schematic is the top), carried by its `<cell>__top` bench copy.
    BenchCopy,
    /// A netlist text instance with `spice_ignore=true`: xschem does not netlist it.
    SpiceIgnore,
}

impl Drop {
    /// Returns the report word.
    pub fn word(self) -> &'static str {
        match self {
            Drop::Malformed => "malformed",
            Drop::NotACell => "not-a-cell",
            Drop::MappedToCore => "mapped-to-core",
            Drop::Graphic => "schematic-graphic",
            Drop::Display => "display-only",
            Drop::ZeroWire => "zero-length-wire",
            Drop::NonElectrical => "non-electrical",
            Drop::OtherNetlister => "other-netlister-text",
            Drop::SymbolNetlistRecord => "symbol-wire-or-instance",
            Drop::Layer => "layer-out-of-range",
            Drop::Embedded => "embedded-symbol",
            Drop::BenchCopy => "in-bench-copy",
            Drop::SpiceIgnore => "spice-ignore",
        }
    }
}

/// What a report item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// `needs_review`: not translated; the field stays at its default, the cell is
    /// flagged, nothing is guessed into a netlist.
    Review(Reason),
    /// Dropped on purpose (no netlist effect).
    Dropped(Drop),
    /// A coordinate, scale or angle rounded onto the milli grid (D33).
    Rounding,
    /// An unresolved `@token`: rendered empty, as xschem does.
    Token,
    /// A generated alternate-schematic cell (`<cell>__<alt>`).
    AltCell,
}

impl Class {
    /// Returns the report word.
    pub fn word(self) -> String {
        match self {
            Class::Review(r) => format!("needs_review:{}", r.word()),
            Class::Dropped(d) => format!("dropped:{}", d.word()),
            Class::Rounding => "rounding".into(),
            Class::Token => "unknown-token".into(),
            Class::AltCell => "alt-cell".into(),
        }
    }
}

/// One report row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Row in [`Report::files`].
    pub file: u32,
    /// 1-based source line (0 when the item concerns the whole file).
    pub line: u32,
    /// What it is.
    pub class: Class,
    /// Input records this item accounts for (0 for a field-level item).
    pub records: u32,
    /// The construct or the reason in words.
    pub detail: String,
}

/// Everything the importer reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Input paths, sorted; items index this.
    pub files: Vec<String>,
    /// Input records per file (top level plus embedded bodies).
    pub records_in: Vec<u32>,
    /// Records per file that became output (emitted as records or folded into one,
    /// such as a `K` block into a symbol interface).
    pub records_out: Vec<u32>,
    /// Report rows in discovery order.
    pub items: Vec<Item>,
    /// Output files written, sorted.
    pub outputs: Vec<String>,
    /// Instance attributes that are neither parameters nor netlist controls, by key,
    /// with their counts (footprints, `sig_type`, …); dropped.
    pub unused_attrs: BTreeMap<String, u32>,
    /// Instances of xschem `devices/` cell symbols seen.
    pub device_instances: u32,
    /// Of those, instances mapped to a `core` cell.
    pub core_instances: u32,
}

impl Report {
    /// Returns the records per file that some item accounts for.
    pub fn records_reported(&self) -> Vec<u32> {
        let mut r = vec![0u32; self.files.len()];
        for it in &self.items {
            r[it.file as usize] += it.records;
        }
        r
    }

    /// Asserts count conservation for every file: records in = out + reported.
    ///
    /// # Panics
    ///
    /// Panics naming the first file whose counts do not add up: that is an importer
    /// bug (a record neither translated nor reported).
    pub fn assert_conservation(&self) {
        assert_eq!(self.files.len(), self.records_in.len());
        assert_eq!(self.files.len(), self.records_out.len());
        let reported = self.records_reported();
        for (i, f) in self.files.iter().enumerate() {
            assert_eq!(
                self.records_in[i],
                self.records_out[i] + reported[i],
                "count conservation broken for {f}: in {} out {} reported {}",
                self.records_in[i],
                self.records_out[i],
                reported[i]
            );
        }
    }

    /// Returns item counts by class.
    pub fn counts(&self) -> BTreeMap<Class, u32> {
        let mut m = BTreeMap::new();
        for it in &self.items {
            *m.entry(it.class).or_insert(0) += 1;
        }
        m
    }

    /// Renders `import-report.md`.
    pub fn markdown(&self) -> String {
        let mut s = String::new();
        let (rin, rout): (u64, u64) = (
            self.records_in.iter().map(|&n| u64::from(n)).sum(),
            self.records_out.iter().map(|&n| u64::from(n)).sum(),
        );
        let rep: u64 = self.items.iter().map(|i| u64::from(i.records)).sum();
        let _ = writeln!(s, "# Import report\n");
        let _ = writeln!(s, "| Count | Value |\n|---|---|");
        let _ = writeln!(s, "| input files | {} |", self.files.len());
        let _ = writeln!(s, "| output files | {} |", self.outputs.len());
        let _ = writeln!(s, "| records in | {rin} |");
        let _ = writeln!(s, "| records out (emitted or folded) | {rout} |");
        let _ = writeln!(s, "| records reported | {rep} |");
        let _ = writeln!(
            s,
            "| devices instances mapped to core | {} / {} |",
            self.core_instances, self.device_instances
        );
        let _ = writeln!(
            s,
            "\nCount conservation: records in = out + reported holds per file.\n"
        );
        let _ = writeln!(s, "## Items by class\n\n| Class | Items |\n|---|---|");
        for (c, n) in self.counts() {
            let _ = writeln!(s, "| {} | {n} |", c.word());
        }
        let _ = writeln!(s, "\n## needs_review\n");
        self.table(&mut s, |c| matches!(c, Class::Review(_)));
        let _ = writeln!(
            s,
            "\n## Generated alternate-schematic cells and unknown tokens\n"
        );
        self.table(&mut s, |c| matches!(c, Class::AltCell | Class::Token));
        let _ = writeln!(s, "\n## Dropped constructs\n");
        self.table(&mut s, |c| matches!(c, Class::Dropped(_)));
        let _ = writeln!(s, "\n## Coordinate roundings (D33)\n");
        self.table(&mut s, |c| matches!(c, Class::Rounding));
        let _ = writeln!(
            s,
            "\n## Unused instance attributes\n\n| Key | Count |\n|---|---|"
        );
        for (k, n) in &self.unused_attrs {
            let _ = writeln!(s, "| `{}` | {n} |", k.replace('|', "\\|"));
        }
        s
    }

    fn table(&self, s: &mut String, keep: impl Fn(Class) -> bool) {
        let _ = writeln!(
            s,
            "| File:line | Class | Records | Detail |\n|---|---|---|---|"
        );
        for it in self.items.iter().filter(|i| keep(i.class)) {
            let detail: String = it
                .detail
                .chars()
                .take(160)
                .map(|c| match c {
                    '\n' | '\r' => ' ',
                    '|' => '¦',
                    c => c,
                })
                .collect();
            let _ = writeln!(
                s,
                "| {}:{} | {} | {} | `{}` |",
                self.files[it.file as usize],
                it.line,
                it.class.word(),
                it.records,
                detail.replace('`', "'")
            );
        }
    }

    /// Renders `import-report.txt`: one tab-separated line per item
    /// (`class file line records detail`, newlines in details escaped as `\n`), after
    /// a `files`/`records` header block.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for (i, f) in self.files.iter().enumerate() {
            let _ = writeln!(
                s,
                "file\t{f}\t{}\t{}",
                self.records_in[i], self.records_out[i]
            );
        }
        for it in &self.items {
            let _ = writeln!(
                s,
                "{}\t{}\t{}\t{}\t{}",
                it.class.word(),
                self.files[it.file as usize],
                it.line,
                it.records,
                it.detail
                    .replace('\\', "\\\\")
                    .replace('\n', "\\n")
                    .replace('\t', "\\t")
            );
        }
        for (k, n) in &self.unused_attrs {
            let _ = writeln!(s, "unused-attr\t{k}\t{n}");
        }
        s
    }
}
