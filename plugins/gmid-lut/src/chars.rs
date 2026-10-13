//! Characterization: the `gmid.chars` project config, the sweep netlist generated from it (for
//! ngspice or ESPice, [`Sim`]), the parsers for the simulator's output (ngspice `wrdata`,
//! ESPice binary raw), and the LUT built from that output.
//!
//! Pure text in, text or [`Lut`] out; the plugin glue does the I/O (fs_write the netlist,
//! spawn the simulator, fs_read the data), and `examples/characterize.rs` does the same
//! natively. Config syntax and both sweeps are specified in the crate README.

use schemify_abi::text::{self, BodyLine};

use crate::lut::{self, Lut, Polarity};

/// Project-relative path of the config.
pub const CHARS_PATH: &str = "gmid.chars";
/// Project-relative directory for generated netlists and simulator output.
pub const WORK_DIR: &str = ".schemify/gmid/work";
/// VGS steps from 0 to VDD (25 mV at 1.8 V).
pub const VGS_STEPS: u16 = 72;
/// VDS steps; VDS runs from VDD/12 to VDD.
pub const VDS_STEPS: u16 = 12;
/// Most L values one device may sweep.
pub const L_MAX: usize = 64;
/// Most rows a `wrdata` file may hold (bounds the parse before any allocation grows).
const ROWS_MAX: usize = 1 << 20;
/// The project root seen from [`WORK_DIR`]: ESPice resolves `.include` paths against the
/// including file's directory, never the working directory, so its deck searches here.
const ROOT_FROM_WORK: &str = "../../..";
/// Frequency (Hz) of ESPice's one-point AC analysis per bias point; Cgg = Im(i_g) / (2π f).
pub const F_AC: f64 = 1e6;
/// Most lengths one ESPice characterization sweeps: its one raw file (≈ 1.8 MB at 8) must
/// parse in one plugin event's fuel (≈ 74 M of the 10^8 default measured, `tests/guest.rs`).
// ponytail: one deck for all lengths; split it into decks of ≤ 8 lengths run in sequence
// (one raw file and event each) when a device needs more.
pub const ESPICE_L_MAX: usize = 8;
/// Most variables one raw-file plot may declare, and most header lines beyond them.
const RAW_VARS_MAX: usize = 4096;

/// The simulator a device is characterized with (`sim` attribute, crate README).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sim {
    /// ngspice: a `.control` nested DC sweep saving the model's OP vectors, `wrdata` per L.
    Ngspice,
    /// ESPice: 0 V ammeters, `.step` over (VDS, VGS) of an `.op` and a one-point `.ac`, one
    /// binary raw file. It has no OP vectors and runs no `.control` block.
    Espice,
}

impl Sim {
    /// The program name, also the `spawn:<name>` grant it needs.
    pub fn program(self) -> &'static str {
        match self {
            Sim::Ngspice => "ngspice",
            Sim::Espice => "espice",
        }
    }
}

/// One `device` record of `gmid.chars`.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// Name the `gmid.*` functions take as their first argument.
    pub name: String,
    /// Project-relative LUT file (default `.schemify/gmid/<name>.glut`).
    pub lut: String,
    /// How to characterize it; `None` for a device that only loads an existing LUT.
    pub sweep: Option<Sweep>,
}

/// Characterization attributes of a [`Device`].
#[derive(Clone, Debug, PartialEq)]
pub struct Sweep {
    /// Project-relative SPICE file with the model includes and options (`.option scale`, …).
    pub deck: String,
    /// Instance line template; `{n}` instance number, `{l}` / `{w}` in µm; nodes `d g s b`.
    pub inst: String,
    /// OP-parameter vector prefix template (`@m.x{n}.msky130_fd_pr__nfet_01v8`); the ngspice
    /// sweep saves `<prefix>[<id>]`, `[gm]`, `[gds]`, `[cgg]`. Unused by ESPice (may be empty).
    pub op: String,
    /// OP name of the drain current: `id` (BSIM4, the default), `ids` (BSIM-CMG via OSDI).
    pub id: String,
    /// Channel type: p sweeps negative VGS/VDS and stores magnitudes.
    pub polarity: Polarity,
    /// Supply (V); VGS sweeps 0..VDD, VDS VDD/12..VDD, queries at VDD/2.
    pub vdd: f64,
    /// Lengths in µm, ascending (the units `{l}` is written in).
    pub l_um: Vec<f64>,
    /// Width in µm every L is simulated at; results are normalized per metre.
    pub w_um: f64,
    /// Free provenance text (PDK, corner, temperature), stored in the LUT.
    pub note: String,
    /// The simulator.
    pub sim: Sim,
}

/// Parses `gmid.chars`: `device <name>` records with indented attributes (crate README).
///
/// # Errors
///
/// `"<record>: <reason>"` for the first malformed record, unknown word or attribute, bad
/// value, duplicate device, or a characterization attribute without `deck`.
pub fn parse(src: &str) -> Result<Vec<Device>, String> {
    let mut out: Vec<Device> = Vec::new();
    for chunk in text::chunks(src.as_bytes()) {
        let chunk = std::str::from_utf8(chunk).map_err(|_| "gmid.chars is not UTF-8".to_owned())?;
        let mut lines = text::lines(chunk);
        let head = lines.next().unwrap_or_default();
        let fail = |why: &str| format!("{head}: {why}");
        let (word, rest) = text::split_word(head).map_err(|_| fail("malformed record"))?;
        if word != "device" {
            return Err(fail("unknown record (only `device`)"));
        }
        let mut v = text::values(rest);
        let name = v
            .need()
            .map_err(|_| fail("missing device name"))?
            .into_owned();
        v.end().map_err(|_| fail("extra values"))?;
        if !text::is_word(&name) || name.len() > lut::NAME_MAX {
            return Err(fail("device name must be a word"));
        }
        if out.iter().any(|d| d.name == name) {
            return Err(fail("duplicate device"));
        }
        if out.len() >= lut::DEVICES_MAX {
            return Err(fail("too many devices"));
        }
        let mut a = Attrs::default();
        for line in lines {
            match text::body_line(line).map_err(|_| fail("malformed attribute line"))? {
                BodyLine::Blank | BodyLine::Comment => {}
                BodyLine::Verbatim(_) => return Err(fail("unexpected `|` line")),
                BodyLine::Attr(key, vals) => a.set(key, vals).map_err(|e| fail(&e))?,
            }
        }
        let lut = a
            .lut
            .take()
            .unwrap_or_else(|| format!(".schemify/gmid/{name}.glut"));
        if !project_path(&lut) {
            return Err(fail("lut must be a project-relative path"));
        }
        let sweep = a.sweep().map_err(|e| fail(&e))?;
        out.push(Device { name, lut, sweep });
    }
    Ok(out)
}

#[derive(Default)]
struct Attrs {
    lut: Option<String>,
    deck: Option<String>,
    inst: Option<String>,
    op: Option<String>,
    id: Option<String>,
    polarity: Option<Polarity>,
    vdd: Option<f64>,
    l: Option<Vec<f64>>,
    w: Option<f64>,
    note: Option<String>,
    sim: Option<Sim>,
}

impl Attrs {
    fn set(&mut self, key: &str, vals: &str) -> Result<(), String> {
        let mut v = text::values(vals);
        let one = |v: &mut text::Values<'_>| -> Result<String, String> {
            let s = v
                .need()
                .map_err(|_| format!("`{key}` needs a value"))?
                .into_owned();
            v.end().map_err(|_| format!("`{key}` takes one value"))?;
            Ok(s)
        };
        let num = |s: &str| -> Result<f64, String> {
            match s.parse::<f64>() {
                Ok(x) if x.is_finite() && x > 0.0 => Ok(x),
                _ => Err(format!("`{key}`: {s:?} is not a positive number")),
            }
        };
        let slot_taken = match key {
            "lut" => self.lut.replace(one(&mut v)?).is_some(),
            "deck" => self.deck.replace(one(&mut v)?).is_some(),
            "inst" => self.inst.replace(one(&mut v)?).is_some(),
            "op" => self.op.replace(one(&mut v)?).is_some(),
            "id" => {
                let id = one(&mut v)?;
                if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    return Err(format!("`id`: {id:?} is not an OP parameter name"));
                }
                self.id.replace(id).is_some()
            }
            "note" => self.note.replace(one(&mut v)?).is_some(),
            "sim" => {
                let sim = match one(&mut v)?.as_str() {
                    "ngspice" => Sim::Ngspice,
                    "espice" => Sim::Espice,
                    _ => return Err("`sim` is ngspice or espice".to_owned()),
                };
                self.sim.replace(sim).is_some()
            }
            "polarity" => {
                let p = match one(&mut v)?.as_str() {
                    "n" => Polarity::N,
                    "p" => Polarity::P,
                    _ => return Err("`polarity` is n or p".to_owned()),
                };
                self.polarity.replace(p).is_some()
            }
            "vdd" => self.vdd.replace(num(&one(&mut v)?)?).is_some(),
            "w" => self.w.replace(num(&one(&mut v)?)?).is_some(),
            "l" => {
                let mut ls = Vec::new();
                for s in v.by_ref() {
                    let s = s.map_err(|_| "`l`: malformed value".to_owned())?;
                    if ls.len() >= L_MAX {
                        return Err(format!("`l`: at most {L_MAX} lengths"));
                    }
                    ls.push(num(&s)?);
                }
                if ls.is_empty() {
                    return Err("`l` needs at least one length".to_owned());
                }
                ls.sort_by(f64::total_cmp);
                if ls.windows(2).any(|w| w[0] == w[1]) {
                    return Err("`l`: duplicate length".to_owned());
                }
                self.l.replace(ls).is_some()
            }
            _ => return Err(format!("unknown attribute `{key}`")),
        };
        if slot_taken {
            return Err(format!("`{key}` given twice"));
        }
        Ok(())
    }

    fn sweep(self) -> Result<Option<Sweep>, String> {
        let Some(deck) = self.deck else {
            let any = self.inst.is_some()
                || self.op.is_some()
                || self.id.is_some()
                || self.sim.is_some()
                || self.vdd.is_some()
                || self.l.is_some();
            return if any {
                Err("characterization attributes need `deck`".to_owned())
            } else {
                Ok(None)
            };
        };
        if !project_path(&deck) {
            return Err("deck must be a project-relative path".to_owned());
        }
        let need =
            |x: Option<String>, k: &str| x.ok_or_else(|| format!("characterization needs `{k}`"));
        let sim = self.sim.unwrap_or(Sim::Ngspice);
        let inst = need(self.inst, "inst")?;
        if !inst.contains("{n}") || !inst.contains("{l}") {
            return Err("`inst` needs {n} and {l}".to_owned());
        }
        // ngspice reads the OP vectors `op` names; ESPice has none (it measures through
        // ammeters), so `op` and `id` are optional there and ignored, which keeps switching
        // `sim` a one-line edit.
        let op = match sim {
            Sim::Ngspice => need(self.op, "op")?,
            Sim::Espice => self.op.unwrap_or_default(),
        };
        if sim == Sim::Ngspice && !op.contains("{n}") {
            return Err("`op` needs {n}".to_owned());
        }
        // The ESPice deck rewires two copies of each instance (crate README): their nodes
        // must be the template's first four words after the name.
        if sim == Sim::Espice
            && inst
                .split_whitespace()
                .skip(1)
                .take(4)
                .ne(["d", "g", "s", "b"])
        {
            return Err(
                "`inst` for espice must connect `d g s b`, in that order, first".to_owned(),
            );
        }
        if sim == Sim::Espice && self.l.as_ref().is_some_and(|l| l.len() > ESPICE_L_MAX) {
            return Err(format!(
                "`l`: espice characterizes at most {ESPICE_L_MAX} lengths"
            ));
        }
        let vdd = self.vdd.ok_or("characterization needs `vdd`")?;
        if vdd > 100.0 {
            return Err("`vdd` above 100 V".to_owned());
        }
        Ok(Some(Sweep {
            deck,
            inst,
            op,
            id: self.id.unwrap_or_else(|| "id".to_owned()),
            polarity: self.polarity.unwrap_or(Polarity::N),
            vdd,
            l_um: self.l.ok_or("characterization needs `l`")?,
            w_um: self.w.unwrap_or(1.0),
            note: self.note.unwrap_or_default(),
            sim,
        }))
    }
}

/// Mirrors the host's project-relative path rule (plugin-abi §5), so a bad config fails here
/// with a readable message instead of an `E_DENIED` later.
fn project_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 8192
        && !p.starts_with('/')
        && !p.contains(['\\', ':'])
        && p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

/// Project-relative netlist path of device `name`.
pub fn netlist_path(name: &str) -> String {
    format!("{WORK_DIR}/{name}.sp")
}

/// Project-relative `wrdata` output path of instance `n` (1-based, the `n`-th shortest L) of
/// device `name`; ngspice writes it verbatim.
pub fn data_path(name: &str, n: usize) -> String {
    format!("{WORK_DIR}/{name}.{n}.data")
}

/// Project-relative raw file ESPice writes for device `name` ([`Sim::Espice`]).
pub fn raw_path(name: &str) -> String {
    format!("{WORK_DIR}/{name}.raw")
}

/// The command line that simulates device `name`'s netlist, run from the project root:
/// `ngspice -b <netlist>`, or `espice -b --tokenizer=ngspice -r <raw> <netlist>` (binary raw,
/// [`raw_path`]).
pub fn spawn_argv(name: &str, s: &Sweep) -> Vec<String> {
    let mut argv = vec![s.sim.program().to_owned(), "-b".to_owned()];
    if s.sim == Sim::Espice {
        argv.extend([
            "--tokenizer=ngspice".to_owned(),
            "-r".to_owned(),
            raw_path(name),
        ]);
    }
    argv.push(netlist_path(name));
    assert!(argv.len() == 3 || argv.len() == 6);
    argv
}

/// The sweep grid: `(VGS values, VDS values)`, signed by polarity. VGS runs 0..=VDD in
/// [`VGS_STEPS`] steps, VDS VDD/[`VDS_STEPS`]..=VDD.
pub fn grid(s: &Sweep) -> (Vec<f64>, Vec<f64>) {
    let sign = if s.polarity == Polarity::P { -1.0 } else { 1.0 };
    // `vdd · (k / steps)`: the last point is VDD exactly.
    let at = |k: u16, steps: u16| sign * s.vdd * (f64::from(k) / f64::from(steps));
    let vgs: Vec<f64> = (0..=VGS_STEPS).map(|k| at(k, VGS_STEPS)).collect();
    let vds: Vec<f64> = (1..=VDS_STEPS).map(|k| at(k, VDS_STEPS)).collect();
    assert!(vgs.len() == usize::from(VGS_STEPS) + 1 && vds.len() == usize::from(VDS_STEPS));
    (vgs, vds)
}

/// The netlist that characterizes `name` with `s.sim` (crate README, "The sweep"): ngspice
/// writes one `wrdata` file per L ([`data_path`], [`parse_block`]), ESPice one binary raw file
/// ([`raw_path`], [`parse_espice`]).
///
/// # Panics
///
/// If `s` breaks what [`parse`] guarantees (no length, more than [`L_MAX`], `vdd ≤ 0`).
pub fn netlist(name: &str, s: &Sweep) -> String {
    match s.sim {
        Sim::Ngspice => ngspice_netlist(name, s),
        Sim::Espice => espice_netlist(name, s),
    }
}

/// The ngspice batch netlist: every L as a parallel instance on shared nodes, one nested DC
/// sweep (VGS inner, VDS outer), and per instance a `wrdata` of its four OP vectors into
/// [`data_path`]. One file per L keeps each file's parse inside one plugin event's fuel.
/// Paths are relative to the project root (ngspice's cwd).
fn ngspice_netlist(name: &str, s: &Sweep) -> String {
    assert!(!s.l_um.is_empty() && s.l_um.len() <= L_MAX && s.vdd > 0.0);
    let sign = if s.polarity == Polarity::P { -1.0 } else { 1.0 };
    let (vg_step, vd_step) = (s.vdd / f64::from(VGS_STEPS), s.vdd / f64::from(VDS_STEPS));
    let mut t = format!("* gm/Id characterization of {name} (generated by the gmid-lut plugin)\n");
    t.push_str(&format!(".include \"{}\"\n", s.deck));
    for (i, l) in s.l_um.iter().enumerate() {
        let line = s
            .inst
            .replace("{n}", &(i + 1).to_string())
            .replace("{l}", &l.to_string());
        t.push_str(&line.replace("{w}", &s.w_um.to_string()));
        t.push('\n');
    }
    t.push_str(
        "Vgs g 0 0\nVds d 0 0\nVs s 0 0\nVb b 0 0\n.control\nset wr_singlescale\nset wr_vecnames\n",
    );
    let vectors: Vec<String> = (1..=s.l_um.len()).map(|n| vectors(s, n)).collect();
    t.push_str(&format!("save {}\n", vectors.join(" ")));
    t.push_str(&format!(
        "dc Vgs 0 {} {} Vds {} {} {}\n",
        sign * s.vdd,
        sign * vg_step,
        sign * vd_step,
        sign * s.vdd,
        sign * vd_step
    ));
    for (i, v) in vectors.iter().enumerate() {
        t.push_str(&format!("wrdata {} {v}\n", data_path(name, i + 1)));
    }
    t.push_str("quit\n.endc\n.end\n");
    t
}

/// The ESPice netlist (espice.md §4c recipe ii): per L two copies of the instance behind 0 V
/// ammeters, copy a with AC 1 on its gate (Id = i(vda) at the OP, gm = Re i(vda), Cgg =
/// Im i(vga) / 2πf), copy b with AC 1 on its drain (gds = Re i(vdb)); `.op` and a one-point
/// `.ac` at [`F_AC`], stepped over VDS (outer) and VGS (inner: ESPice varies the last `.step`
/// fastest). Paths resolve through `.option search` from [`WORK_DIR`] up to the project root.
fn espice_netlist(name: &str, s: &Sweep) -> String {
    assert!(!s.l_um.is_empty() && s.l_um.len() <= L_MAX && s.vdd > 0.0);
    let (vgs, vds) = grid(s);
    // The first line is the raw file's `Title:` of every plot: short, without the name.
    let mut t = format!(
        "* gmid-lut espice sweep\n* gm/Id characterization of {name} (generated by the gmid-lut plugin)\n\
         .option search=\"{ROOT_FROM_WORK}\"\n.include \"{}\"\n.param vgs=0 vds=0\n",
        s.deck
    );
    let mut save = String::new();
    for (i, l) in s.l_um.iter().enumerate() {
        let n = i + 1;
        let line = s
            .inst
            .replace("{l}", &l.to_string())
            .replace("{w}", &s.w_um.to_string());
        // Copy a: drain and gate ammeters; copy b: drain ammeter, gate on the shared node.
        let a = rewire(
            &line.replace("{n}", &format!("a{n}")),
            &format!("da{n}"),
            &format!("ga{n}"),
        );
        let b = rewire(
            &line.replace("{n}", &format!("b{n}")),
            &format!("db{n}"),
            "gb",
        );
        t.push_str(&format!(
            "Vda{n} da da{n} 0\nVga{n} ga ga{n} 0\n{a}\nVdb{n} db db{n} 0\n{b}\n"
        ));
        save.push_str(&format!(" i(vda{n}) i(vga{n}) i(vdb{n})"));
    }
    t.push_str(
        "Vgsa ga 0 dc {vgs} ac 1\nVdsa da 0 dc {vds}\nVgsb gb 0 dc {vgs}\n\
         Vdsb db 0 dc {vds} ac 1\nVs s 0 0\nVb b 0 0\n",
    );
    t.push_str(&format!(".save{save}\n.op\n.ac lin 1 {F_AC:e} {F_AC:e}\n"));
    let list = |v: &[f64]| v.iter().map(|x| format!(" {x}")).collect::<String>();
    t.push_str(&format!(".step param vds list{}\n", list(&vds)));
    t.push_str(&format!(".step param vgs list{}\n.end\n", list(&vgs)));
    t
}

/// `line` (an `inst` template, nodes `d g s b` first after the name, as [`parse`] checks)
/// with its drain and gate nodes replaced; words are rejoined with single spaces.
fn rewire(line: &str, d: &str, g: &str) -> String {
    let words: Vec<&str> = line.split_whitespace().collect();
    assert!(words.len() >= 5 && words[1] == "d" && words[2] == "g");
    let mut out = String::with_capacity(line.len() + 8);
    for (k, w) in words.iter().enumerate() {
        if k > 0 {
            out.push(' ');
        }
        out.push_str(match k {
            1 => d,
            2 => g,
            _ => w,
        });
    }
    out
}

/// Instance `n`'s saved vectors: `<op>[<id>] <op>[gm] <op>[gds] <op>[cgg]`.
fn vectors(s: &Sweep, n: usize) -> String {
    let prefix = s.op.replace("{n}", &n.to_string());
    let v: Vec<String> = [s.id.as_str(), "gm", "gds", "cgg"]
        .iter()
        .map(|q| format!("{prefix}[{q}]"))
        .collect();
    v.join(" ")
}

/// One instance's sweep: the VGS scale column and `[id, gm, gds, cgg]` per row (VGS inner,
/// VDS outer), as ngspice wrote them (signed).
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// VGS of each row.
    pub scale: Vec<f64>,
    /// `[id, gm, gds, cgg]` of each row.
    pub q: Vec<[f64; 4]>,
}

/// Parses one [`data_path`] file: `wrdata` output with `wr_singlescale` + `wr_vecnames`, an
/// optional header row, then rows of 5 numbers.
///
/// # Errors
///
/// A row without exactly 5 numbers, a non-numeric or non-finite value, no rows, or more than
/// 2^20 rows.
pub fn parse_block(src: &str) -> Result<Block, String> {
    let mut b = Block {
        scale: Vec::new(),
        q: Vec::new(),
    };
    for (k, line) in src.lines().enumerate() {
        let mut toks = line.split_ascii_whitespace();
        let Some(first) = toks.next() else { continue };
        let Some(x) = number(first) else {
            if k == 0 && toks.count() == 4 {
                continue; // header: `v-sweep` + four vector names
            }
            return Err(format!("wrdata line {}: bad row", k + 1));
        };
        if b.scale.len() >= ROWS_MAX {
            return Err("wrdata has too many rows".to_owned());
        }
        let mut q = [0.0; 4];
        for slot in &mut q {
            *slot = toks
                .next()
                .and_then(number)
                .ok_or_else(|| format!("wrdata line {}: bad or missing number", k + 1))?;
        }
        if toks.next().is_some() {
            return Err(format!("wrdata line {}: more than 5 columns", k + 1));
        }
        b.scale.push(x);
        b.q.push(q);
    }
    if b.scale.is_empty() {
        return Err("wrdata has no rows".to_owned());
    }
    debug_assert_eq!(b.scale.len(), b.q.len());
    Ok(b)
}

/// A finite number as ngspice writes it, or `None`.
fn number(tok: &str) -> Option<f64> {
    tok.parse::<f64>().ok().filter(|x| x.is_finite())
}

/// One plot of an ngspice binary raw file, borrowed from it.
#[derive(Clone, Copy, Debug)]
struct Plot<'a> {
    /// The `.step` point of the plot name, `vds=…, vgs=…` (inside its parentheses).
    point: &'a str,
    complex: bool,
    /// The `Variables:` lines, verbatim.
    vars: &'a [u8],
    nvars: usize,
    /// The header after the `Plotname` line, through `Binary:\n`: plots of one kind in one
    /// deck repeat it byte for byte ([`next_plot_like`]).
    tail: &'a [u8],
    /// One point: `nvars` doubles, or `nvars` (re, im) pairs when complex.
    data: &'a [u8],
}

impl Plot<'_> {
    /// Value `k` (the real part when complex).
    fn re(&self, k: usize) -> f64 {
        self.f64_at(if self.complex { 2 * k } else { k })
    }

    /// The imaginary part of value `k` of a complex plot.
    fn im(&self, k: usize) -> f64 {
        assert!(self.complex);
        self.f64_at(2 * k + 1)
    }

    fn f64_at(&self, i: usize) -> f64 {
        let b: [u8; 8] = self.data[8 * i..8 * i + 8].try_into().expect("8 bytes");
        f64::from_le_bytes(b)
    }

    /// The index of variable `name`, from the `Variables:` lines (`\t<k>\t<name>\t<type>`).
    fn var(&self, name: &str) -> Result<usize, String> {
        let text = std::str::from_utf8(self.vars).map_err(|_| "raw variables not UTF-8")?;
        let k = text
            .lines()
            .position(|l| l.split_whitespace().nth(1) == Some(name))
            .ok_or_else(|| format!("raw file has no {name}"))?;
        assert!(k < self.nvars);
        Ok(k)
    }
}

/// Reads the plot starting at `raw[*at..]` and moves `at` past it. Header lines other than
/// `Plotname`, `Flags`, `No. Variables`, `No. Points`, `Variables` and `Binary` are skipped.
///
/// # Errors
///
/// A truncated or non-UTF-8 header, an ASCII (`Values:`) plot, a plot of other than one
/// point, more than [`RAW_VARS_MAX`] variables, a variable count that disagrees with the
/// lines, or too few data bytes.
fn next_plot<'a>(raw: &'a [u8], at: &mut usize) -> Result<Plot<'a>, String> {
    let (mut name, mut complex, mut nvars, mut points) = ("", false, None, None);
    let (mut vars_at, mut vars_lines, mut tail_at) = (None, 0usize, None);
    let vars_end = loop {
        let rest = &raw[*at..];
        let len = rest
            .iter()
            .position(|&b| b == b'\n')
            .ok_or("raw header truncated")?;
        let line = std::str::from_utf8(&rest[..len]).map_err(|_| "raw header not UTF-8")?;
        let start = *at;
        *at += len + 1;
        if vars_at.is_some() && line.starts_with(['\t', ' ']) {
            vars_lines += 1;
            if vars_lines > RAW_VARS_MAX {
                return Err("raw plot has too many variables".to_owned());
            }
            continue;
        }
        let count = |v: &str| {
            v.trim()
                .parse::<usize>()
                .ok()
                .filter(|n| *n <= RAW_VARS_MAX)
        };
        if let Some(v) = line.strip_prefix("Plotname: ") {
            name = v;
            tail_at = Some(*at);
        } else if let Some(v) = line.strip_prefix("Flags: ") {
            complex = v.contains("complex");
        } else if let Some(v) = line.strip_prefix("No. Variables: ") {
            nvars = Some(count(v).ok_or("bad raw variable count")?);
        } else if let Some(v) = line.strip_prefix("No. Points: ") {
            points = Some(count(v).ok_or("bad raw point count")?);
        } else if line == "Variables:" {
            vars_at = Some(*at);
        } else if line == "Binary:" {
            break start;
        } else if line == "Values:" {
            return Err("ASCII raw file: run espice with --format=binary".to_owned());
        }
    };
    let nvars = nvars.ok_or("raw plot without `No. Variables`")?;
    if points != Some(1) || nvars == 0 || vars_lines != nvars {
        return Err(format!(
            "raw plot `{name}`: want 1 point and {nvars} variable lines"
        ));
    }
    let tail = &raw[tail_at.ok_or("raw plot without `Plotname`")?..*at];
    let bytes = nvars * if complex { 16 } else { 8 };
    let data = raw.get(*at..*at + bytes).ok_or("raw data truncated")?;
    *at += bytes;
    let vars = &raw[vars_at.expect("counted variable lines")..vars_end];
    Ok(Plot {
        point: step_point(name),
        complex,
        vars,
        nvars,
        tail,
        data,
    })
}

/// Reads the plot at `raw[*at..]` when it is laid out like `like`: only the lines up to its
/// `Plotname` are scanned; the rest of its header must equal `like.tail` byte for byte, so
/// its variables, flags and size are `like`'s. Why: a stepped deck repeats one header per
/// point (≈ 1 KB with 8 lengths), and parsing each line again cost 2× the plugin's fuel per
/// event on a real-size file; a byte comparison costs a fraction of that.
///
/// # Errors
///
/// A truncated header or data, no `Plotname` within 8 lines, or a header that differs from
/// `like`'s.
fn next_plot_like<'a>(raw: &'a [u8], at: &mut usize, like: &Plot<'a>) -> Result<Plot<'a>, String> {
    assert_eq!(
        like.data.len(),
        like.nvars * if like.complex { 16 } else { 8 }
    );
    assert!(like.tail.ends_with(b"Binary:\n"));
    for _ in 0..8 {
        let rest = &raw[*at..];
        let len = rest
            .iter()
            .position(|&b| b == b'\n')
            .ok_or("raw header truncated")?;
        *at += len + 1;
        let Some(name) = rest[..len].strip_prefix(b"Plotname: ") else {
            continue;
        };
        let name = std::str::from_utf8(name).map_err(|_| "raw header not UTF-8")?;
        let tail = raw
            .get(*at..*at + like.tail.len())
            .ok_or("raw header truncated")?;
        // ponytail: the ends only (where a changed variable count would move `Binary:`);
        // comparing the whole ≈ 1 KB per plot cost ~half the event's fuel. Compare it all if
        // a deck ever varies its variables between points.
        let (n, edge) = (tail.len(), tail.len().min(16));
        let same = |a: &[u8], b: &[u8]| a[..edge] == b[..edge] && a[n - edge..] == b[n - edge..];
        if n != like.tail.len() || !same(tail, like.tail) {
            return Err(format!(
                "raw plot `{name}`: header differs from the first plot's"
            ));
        }
        *at += tail.len();
        let data = raw
            .get(*at..*at + like.data.len())
            .ok_or("raw data truncated")?;
        *at += data.len();
        return Ok(Plot {
            point: step_point(name),
            data,
            ..*like
        });
    }
    Err("raw plot without `Plotname`".to_owned())
}

/// The `.step` point of a plot name, `AC Analysis (vds=0.4, vgs=0)` → `vds=0.4, vgs=0`.
fn step_point(name: &str) -> &str {
    name.split_once('(')
        .and_then(|(_, p)| p.strip_suffix(')'))
        .unwrap_or("")
}

/// The value of `key` in a `.step` point text `vds=0.4, vgs=0`.
fn step_value(point: &str, key: &str) -> Option<f64> {
    point.split(',').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        if k == key { number(v) } else { None }
    })
}

/// Parses the binary raw file ESPice wrote for [`netlist`] (`sim espice`, `--format=binary`)
/// into one [`Block`] per L (VGS inner, VDS outer, as [`build`] takes them): per `.step` point
/// an `.op` plot (Id = `i(vda<n>)`) then an `.ac` plot (gm = Re `i(vda<n>)`, gds = Re
/// `i(vdb<n>)`, Cgg = Im `i(vga<n>)` / 2πf), the point read from the plot names and checked
/// against [`grid`].
///
/// Cost: one pass over the file; every plot after the first of its kind must repeat that
/// plot's header (checked at its length and ends), so names are looked up once. A real
/// 8-length file (≈ 1.7 MB) parses and builds in ≈ 73 M fuel, inside one plugin event's 10^8
/// (`tests/guest.rs`).
///
/// # Errors
///
/// A truncated or ASCII file, a plot of other than one point, plots not alternating
/// `.op`/`.ac` on one point, a point off the planned grid or out of order, a header that
/// changes between points, a missing ammeter, a non-finite value, or trailing bytes.
pub fn parse_espice(s: &Sweep, raw: &[u8]) -> Result<Vec<Block>, String> {
    let (vgs, vds) = grid(s);
    let (ng, nl) = (vgs.len(), s.l_um.len());
    let steps = ng * vds.len();
    let tol = 1e-6 * s.vdd;
    let mut blocks: Vec<Block> = (0..nl)
        .map(|_| Block {
            scale: Vec::with_capacity(steps),
            q: Vec::with_capacity(steps),
        })
        .collect();
    /// Where the values sit, read from the first `.op` / `.ac` pair.
    struct Layout<'a> {
        op: Plot<'a>,
        ac: Plot<'a>,
        /// Per L: the `.ac` indices of i(vda), i(vga), i(vdb).
        idx: Vec<[usize; 3]>,
        /// The `.op` index of i(vda1); L `n` is 3n further.
        id0: usize,
        freq: usize,
    }
    let mut first: Option<Layout<'_>> = None;
    let mut at = 0;
    for k in 0..steps {
        let (op, ac) = match &first {
            None => (next_plot(raw, &mut at)?, next_plot(raw, &mut at)?),
            Some(lay) => (
                next_plot_like(raw, &mut at, &lay.op)?,
                next_plot_like(raw, &mut at, &lay.ac)?,
            ),
        };
        if op.complex || !ac.complex || op.point != ac.point {
            return Err(format!(
                "raw plots {}, {}: not an .op/.ac pair",
                2 * k,
                2 * k + 1
            ));
        }
        let (g, d) = (step_value(op.point, "vgs"), step_value(op.point, "vds"));
        let on_grid = |x: Option<f64>, want: f64| x.is_some_and(|x| (x - want).abs() <= tol);
        if !on_grid(g, vgs[k % ng]) || !on_grid(d, vds[k / ng]) {
            return Err(format!(
                "raw point {k} ({}): off the planned grid",
                op.point
            ));
        }
        if first.is_none() {
            let mut idx = Vec::with_capacity(nl);
            for n in 1..=nl {
                let a = ac.var(&format!("i(vda{n})"))?;
                idx.push([
                    a,
                    ac.var(&format!("i(vga{n})"))?,
                    ac.var(&format!("i(vdb{n})"))?,
                ]);
            }
            let (id0, f) = (op.var("i(vda1)")?, ac.var("frequency")?);
            // The `.op` plot lists the saved currents in the same order, without frequency.
            if (1..=nl).any(|n| op.var(&format!("i(vda{n})")).ok() != Some(id0 + 3 * (n - 1))) {
                return Err("raw .op plot: unexpected variable order".to_owned());
            }
            first = Some(Layout {
                op,
                ac,
                idx,
                id0,
                freq: f,
            });
        }
        let lay = first.as_ref().expect("set above");
        let w = 2.0 * std::f64::consts::PI * ac.re(lay.freq);
        for (n, &[a, gi, b]) in lay.idx.iter().enumerate() {
            let q = [op.re(lay.id0 + 3 * n), ac.re(a), ac.re(b), ac.im(gi) / w];
            if q.iter().any(|x| !x.is_finite()) {
                return Err(format!("raw point {k}: non-finite value"));
            }
            blocks[n].scale.push(g.expect("on the grid"));
            blocks[n].q.push(q);
        }
    }
    if at != raw.len() {
        return Err("raw file has plots past the planned grid".to_owned());
    }
    assert!(
        blocks
            .iter()
            .all(|b| b.scale.len() == steps && b.q.len() == steps)
    );
    Ok(blocks)
}

/// The LUT provenance text: simulator, the quantities' definitions, deck and note. ESPice has
/// no OP vectors, so its id and gds are terminal quantities (they include junction leakage and
/// GIDL, visible only in deep subthreshold) and its Cgg, from the gate current, includes the
/// overlap capacitances (on FreePDK45 a median 1.3× ngspice's intrinsic `@m[cgg]` above 1 µA,
/// up to 8× near threshold: `tests/espice.rs`), so its ft is lower (espice.md §4c).
fn provenance(s: &Sweep) -> String {
    let how = match s.sim {
        Sim::Ngspice => "ngspice characterization (OP vectors: channel id, gds; intrinsic cgg)",
        Sim::Espice => {
            "espice characterization (0 V ammeters + 1 MHz AC per bias: terminal id and gds; \
             cgg includes overlap, so ft is lower than from ngspice's intrinsic cgg)"
        }
    };
    format!("{how}; deck {}; {}", s.deck, s.note)
}

/// Builds `name`'s LUT from its sweep blocks (one per L, in [`data_path`] order): VGS from the
/// scale's first run, VDS from the planned steps, values normalized per metre of width
/// (magnitudes).
///
/// # Errors
///
/// A block count other than the L count, blocks of different length or VGS, a row count that
/// is not whole VGS runs, or a table failing [`Lut::check`].
pub fn build(name: &str, s: &Sweep, blocks: &[Block]) -> Result<Lut, String> {
    let nl = s.l_um.len();
    if blocks.len() != nl {
        return Err(format!("{} data blocks for {nl} lengths", blocks.len()));
    }
    let scale = &blocks[0].scale;
    let rows = scale.len();
    if blocks
        .iter()
        .any(|b| b.scale.len() != rows || b.q.len() != rows)
    {
        return Err("data blocks differ in length".to_owned());
    }
    let ng = (1..rows)
        .find(|&i| scale[i].abs() <= scale[i - 1].abs())
        .unwrap_or(rows);
    if ng < 2 || !rows.is_multiple_of(ng) {
        return Err(format!(
            "{rows} rows are not whole VGS sweeps of {ng} points"
        ));
    }
    let nd = rows / ng;
    if ng > lut::AXIS_MAX || nd > lut::AXIS_MAX {
        return Err("sweep too large".to_owned());
    }
    for b in blocks {
        for r in 0..rows {
            if (b.scale[r] - scale[r % ng]).abs() > 1e-6 {
                return Err(format!(
                    "row {r}: VGS {} breaks the sweep pattern",
                    b.scale[r]
                ));
            }
        }
    }
    let w = s.w_um * 1e-6;
    let vd_step = s.vdd / f64::from(VDS_STEPS);
    let mut t = Lut {
        name: name.to_owned(),
        polarity: s.polarity,
        vdd: s.vdd,
        vds_nom: s.vdd / 2.0, // snapped onto the grid below
        provenance: provenance(s),
        l: s.l_um.iter().map(|l| l * 1e-6).collect(),
        vds: (1..=nd).map(|k| vd_step * k as f64).collect(),
        vgs: scale[..ng].iter().map(|v| v.abs()).collect(),
        id_w: Vec::with_capacity(nl * rows),
        gm_w: Vec::with_capacity(nl * rows),
        gds_w: Vec::with_capacity(nl * rows),
        cgg_w: Vec::with_capacity(nl * rows),
    };
    // VDD/2 is a grid point by construction; float steps may miss it by an ulp.
    if let Some(&v) = t.vds.iter().find(|v| (*v - t.vds_nom).abs() < 1e-9 * s.vdd) {
        t.vds_nom = v;
    }
    t.provenance.truncate(lut::PROVENANCE_MAX);
    while !t.provenance.is_char_boundary(t.provenance.len()) {
        t.provenance.pop();
    }
    for b in blocks {
        for q in &b.q {
            let per_w = |k: usize| (q[k].abs() / w) as f32;
            t.id_w.push(per_w(0));
            t.gm_w.push(per_w(1));
            t.gds_w.push(per_w(2));
            t.cgg_w.push(per_w(3));
        }
    }
    t.check().map_err(|e| format!("{name}: {e}"))?;
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHARS: &str = "\
# sky130 devices
device nfet_01v8
  lut gmid/nfet_01v8.glut
  deck models/tt.spice
  inst \"X{n} d g s b sky130_fd_pr__nfet_01v8 W={w} L={l} nf=1\"
  op @m.x{n}.msky130_fd_pr__nfet_01v8
  vdd 1.8
  l 0.5 0.15
  note \"sky130 tt 27C\"

device ext
  lut vendor/ext.glut
";

    #[test]
    fn config_parses_with_defaults_and_rejects_typos() {
        let d = parse(CHARS).unwrap();
        assert_eq!(d.len(), 2);
        let s = d[0].sweep.as_ref().unwrap();
        assert_eq!(
            (s.l_um.as_slice(), s.w_um, s.polarity),
            (&[0.15, 0.5][..], 1.0, Polarity::N)
        );
        assert_eq!(
            d[1],
            Device {
                name: "ext".into(),
                lut: "vendor/ext.glut".into(),
                sweep: None
            }
        );
        assert_eq!(parse("device a\n").unwrap()[0].lut, ".schemify/gmid/a.glut");
        for bad in [
            "devise a\n",
            "device a\n  lutt x\n",
            "device a\n  lut ../x\n",
            "device a\n  lut /abs\n",
            "device a\n  vdd 1.8\n",
            "device a\n  deck d.sp\n",
            "device a\ndevice a\n",
            "device a\n  deck d.sp\n  inst \"X{n} d g s b m L={l}\"\n  op p{n}\n  vdd -1\n  l 1\n",
            "device a\n  lut x\n  lut y\n",
            "device \"a b\"\n",
            "device a\n  id ids\n",
            "device a\n  deck d.sp\n  inst \"N{n} d g s b m l={l}u\"\n  op @n{n}\n  id \"i[d]\"\n  vdd 1\n  l 1\n",
        ] {
            assert!(parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn netlist_sweeps_every_length_and_saves_four_vectors_each() {
        let d = parse(CHARS).unwrap();
        let s = d[0].sweep.as_ref().unwrap();
        let n = netlist(&d[0].name, s);
        assert!(
            n.contains("X1 d g s b sky130_fd_pr__nfet_01v8 W=1 L=0.15 nf=1\n"),
            "{n}"
        );
        assert!(n.contains("X2 d g s b sky130_fd_pr__nfet_01v8 W=1 L=0.5 nf=1\n"));
        assert!(n.contains("dc Vgs 0 1.8 0.025 Vds 0.15 1.8 0.15\n"), "{n}");
        assert!(n.contains(
            "wrdata .schemify/gmid/work/nfet_01v8.1.data @m.x1.msky130_fd_pr__nfet_01v8[id] "
        ));
        assert!(n.contains(
            "wrdata .schemify/gmid/work/nfet_01v8.2.data @m.x2.msky130_fd_pr__nfet_01v8[id] "
        ));
        assert_eq!(
            n.matches("[cgg]").count(),
            4,
            "save + wrdata, two instances"
        );
        let osdi = "device n\n  deck d.sp\n  inst \"N{n} d g s b nmos_rvt nfin=14 l={l}u\"\n  op @n{n}\n  id ids\n  vdd 0.8\n  l 0.021\n";
        let d = parse(osdi).unwrap();
        let n = netlist(&d[0].name, d[0].sweep.as_ref().unwrap());
        assert!(
            n.contains("wrdata .schemify/gmid/work/n.1.data @n1[ids] @n1[gm] @n1[gds] @n1[cgg]\n"),
            "{n}"
        );
        let mut p = s.clone();
        p.polarity = Polarity::P;
        assert!(netlist("p", &p).contains("dc Vgs 0 -1.8 -0.025 Vds -0.15 -1.8 -0.15\n"));
    }

    /// ngspice-shaped output of one instance: VGS 0/0.9/1.8 inner, six VDS steps outer (VDD/2
    /// is the sixth), values scaled by the VDS step so rows differ.
    fn wrdata() -> String {
        let mut t = " v-sweep  @m.x1.m[id] @m.x1.m[gm] @m.x1.m[gds] @m.x1.m[cgg]\n".to_owned();
        for d in 1..=6 {
            for (vg, id, gm) in [(0.0, 1e-12, 3e-11), (0.9, 2e-5, 2e-4), (1.8, 2e-4, 6e-4)] {
                let k = 1.0 + 0.01 * f64::from(d);
                t.push_str(&format!(
                    " {vg:.8e}  {:.8e}  {:.8e}  {:.8e}  {:.8e}\n",
                    id * k,
                    gm * k,
                    1e-6 * k,
                    8e-16 * k
                ));
            }
        }
        t
    }

    #[test]
    fn wrdata_builds_a_normalized_lut_and_rejects_ragged_rows() {
        let mut s = Sweep {
            deck: "d.sp".into(),
            inst: "X{n} d g s b m L={l}".into(),
            op: "@m.x{n}.m".into(),
            id: "id".into(),
            polarity: Polarity::N,
            vdd: 1.8,
            l_um: vec![0.5],
            w_um: 2.0,
            note: String::new(),
            sim: Sim::Ngspice,
        };
        let src = wrdata();
        let block = parse_block(&src).unwrap();
        let t = build("m", &s, std::slice::from_ref(&block)).unwrap();
        assert_eq!((t.l.len(), t.vds.len(), t.vgs.len()), (1, 6, 3));
        assert_eq!(t.vgs, [0.0, 0.9, 1.8]);
        assert!((t.vds[5] - 0.9).abs() < 1e-12 && (t.l[0] - 0.5e-6).abs() < 1e-18);
        // 2e-5·1.01 A over 2 µm = 10.1 A/m at (d 0, g 1).
        assert!((f64::from(t.id_w[t.idx(0, 0, 1)]) - 10.1).abs() < 1e-4);
        assert!((f64::from(t.cgg_w[t.idx(0, 1, 2)]) - 8e-16 * 1.02 / 2e-6).abs() < 1e-12);
        let lines: Vec<&str> = src.lines().collect();
        let ragged = src.replace(lines[4], lines[4].rsplit_once(' ').unwrap().0);
        assert!(parse_block(&ragged).is_err(), "ragged row");
        assert!(
            parse_block(&format!("{src} 1 2 3 4 5 6\n")).is_err(),
            "wide row"
        );
        assert!(parse_block("").is_err() && parse_block("v-sweep a b c d\n").is_err());
        assert!(parse_block(&src.replacen("2.02000000e-5", "nan", 1)).is_err());
        let off = src.replace(
            lines[6],
            &lines[6].replacen("1.80000000e0", "1.70000000e0", 1),
        );
        assert!(
            build("m", &s, &[parse_block(&off).unwrap()]).is_err(),
            "broken VGS pattern"
        );
        let short = Block {
            scale: block.scale[..9].to_vec(),
            q: block.q[..9].to_vec(),
        };
        s.l_um = vec![0.5, 1.0];
        assert!(
            build("m", &s, &[block.clone(), short]).is_err(),
            "blocks differ"
        );
        assert!(
            build("m", &s, &[block]).is_err(),
            "one block for two lengths"
        );
    }

    const ESPICE: &str = "device n45\n  deck gmid/fp45_tt.sp\n  inst \"M{n} d g s b NMOS_VTL w={w}u l={l}u\"\n  \
                          sim espice\n  vdd 0.8\n  l 0.05 1\n  note \"FreePDK45 tt\"\n";

    #[test]
    fn espice_config_needs_rewirable_nodes_and_no_op() {
        let s = parse(ESPICE).unwrap().remove(0).sweep.unwrap();
        assert_eq!((s.sim, s.op.as_str(), s.l_um.len()), (Sim::Espice, "", 2));
        // `op`/`id` may stay when switching back and forth (ignored by espice).
        let both = ESPICE.replace("  vdd", "  op @m{n}\n  id ids\n  vdd");
        assert_eq!(
            parse(&both).unwrap()[0].sweep.as_ref().unwrap().sim,
            Sim::Espice
        );
        for bad in [
            ESPICE.replace("d g s b", "g d s b"),
            ESPICE.replace("M{n} d g s b NMOS_VTL", "M{n} d g s NMOS_VTL"),
            ESPICE.replace("sim espice", "sim spectre"),
            ESPICE.replace("  sim espice\n", "  sim espice\n  sim ngspice\n"),
            // ngspice still needs `op`.
            ESPICE.replace("sim espice", "sim ngspice"),
            "device a\n  sim espice\n".to_owned(),
            ESPICE.replace("l 0.05 1", "l 1 2 3 4 5 6 7 8 9"),
        ] {
            assert!(parse(&bad).is_err(), "accepted {bad:?}");
        }
        assert_eq!(
            spawn_argv("n45", &s),
            [
                "espice",
                "-b",
                "--tokenizer=ngspice",
                "-r",
                ".schemify/gmid/work/n45.raw",
                ".schemify/gmid/work/n45.sp"
            ]
        );
        let mut ng = s.clone();
        ng.sim = Sim::Ngspice;
        assert_eq!(
            spawn_argv("n45", &ng),
            ["ngspice", "-b", ".schemify/gmid/work/n45.sp"]
        );
    }

    /// espice.md §4c recipe (ii), as hand-checked on the real binary: two ammeter-wrapped
    /// copies per L, AC on the gate of copy a and on the drain of copy b, `.op` + one-point
    /// `.ac`, VDS stepped outside VGS, includes found from the project root.
    #[test]
    fn espice_netlist_wraps_two_copies_per_length_in_ammeters() {
        let s = parse(ESPICE).unwrap().remove(0).sweep.unwrap();
        let n = netlist("n45", &s);
        let head = "* gmid-lut espice sweep\n\
                    * gm/Id characterization of n45 (generated by the gmid-lut plugin)\n\
                    .option search=\"../../..\"\n.include \"gmid/fp45_tt.sp\"\n.param vgs=0 vds=0\n\
                    Vda1 da da1 0\nVga1 ga ga1 0\nMa1 da1 ga1 s b NMOS_VTL w=1u l=0.05u\n\
                    Vdb1 db db1 0\nMb1 db1 gb s b NMOS_VTL w=1u l=0.05u\n\
                    Vda2 da da2 0\nVga2 ga ga2 0\nMa2 da2 ga2 s b NMOS_VTL w=1u l=1u\n\
                    Vdb2 db db2 0\nMb2 db2 gb s b NMOS_VTL w=1u l=1u\n\
                    Vgsa ga 0 dc {vgs} ac 1\nVdsa da 0 dc {vds}\nVgsb gb 0 dc {vgs}\n\
                    Vdsb db 0 dc {vds} ac 1\nVs s 0 0\nVb b 0 0\n\
                    .save i(vda1) i(vga1) i(vdb1) i(vda2) i(vga2) i(vdb2)\n.op\n.ac lin 1 1e6 1e6\n";
        assert!(n.starts_with(head), "{n}");
        let steps: Vec<&str> = n[head.len()..].lines().collect();
        assert_eq!(steps.len(), 3, "{n}");
        let vds: Vec<f64> = steps[0]
            .strip_prefix(".step param vds list ")
            .unwrap()
            .split(' ')
            .map(|x| x.parse().unwrap())
            .collect();
        let vgs: Vec<f64> = steps[1]
            .strip_prefix(".step param vgs list ")
            .unwrap()
            .split(' ')
            .map(|x| x.parse().unwrap())
            .collect();
        assert_eq!((vds.len(), vgs.len(), steps[2]), (12, 73, ".end"));
        assert!((vds[0] - 0.8 / 12.0).abs() < 1e-15 && vds[11] == 0.8 && vgs[72] == 0.8);
        assert!(!n.contains(".control") && !n.contains('@'));
        let mut p = s.clone();
        p.polarity = Polarity::P;
        let n = netlist("p", &p);
        assert!(
            n.contains(" -0.8\n.end\n") && n.contains("list -0.06666666666666667 "),
            "{n}"
        );
    }

    /// One `.op` + `.ac` plot pair recorded from ESPice 1.0.0 (`--format=binary`, one FreePDK45
    /// NMOS_VTL L = 50 nm, VDS = 0.8/12, VGS = 0): the header layout and value order the parser
    /// relies on.
    #[test]
    fn recorded_espice_plots_parse() {
        let raw: &[u8] = include_bytes!("../tests/fixtures/espice_op_ac.raw");
        let mut at = 0;
        let op = next_plot(raw, &mut at).unwrap();
        let ac = next_plot(raw, &mut at).unwrap();
        assert_eq!(at, raw.len());
        assert_eq!(
            (op.complex, ac.complex, op.nvars, ac.nvars),
            (false, true, 3, 4)
        );
        assert_eq!(op.point, "vds=0.06666666666666668, vgs=-0");
        assert_eq!(op.point, ac.point);
        assert_eq!(step_value(op.point, "vds"), Some(0.06666666666666668));
        assert_eq!(step_value(op.point, "vgs"), Some(0.0));
        assert_eq!(op.var("i(vda1)"), Ok(0));
        assert_eq!((ac.var("frequency"), ac.var("i(vdb1)")), (Ok(0), Ok(3)));
        assert!(ac.var("i(vda2)").is_err());
        assert_eq!(op.re(0), 1.2134893759369444e-8);
        assert_eq!(ac.re(0), 1e6);
        assert_eq!(
            (ac.re(1), ac.im(1)),
            (3.315537209022122e-7, -2.375947836540056e-9)
        );
        assert_eq!(ac.im(2), 5.755213641958612e-9);
        assert_eq!(ac.re(3), 5.401999876081796e-8);
        // Every truncation fails cleanly; an ASCII plot is refused.
        for cut in 0..raw.len() {
            let mut at = 0;
            let r = next_plot(&raw[..cut], &mut at).and_then(|_| next_plot(&raw[..cut], &mut at));
            assert!(r.is_err(), "cut at {cut}");
        }
        let ascii = b"Plotname: x\nFlags: real\nNo. Variables: 1\nNo. Points: 1\nVariables:\n\t0\tv(a)\tvoltage\nValues:\n0\t1\n";
        assert!(next_plot(ascii, &mut 0).unwrap_err().contains("binary"));
    }

    /// An ESPice-shaped raw file for `s` (the recorded header layout) whose values follow
    /// `f(step, n) -> [id, gm, gds, cgg]`.
    fn espice_raw(s: &Sweep, f: impl Fn(usize, usize) -> [f64; 4]) -> Vec<u8> {
        let (vgs, vds) = grid(s);
        let nl = s.l_um.len();
        let mut raw = Vec::new();
        for k in 0..vgs.len() * vds.len() {
            let point = format!("vds={}, vgs={}", vds[k / vgs.len()], vgs[k % vgs.len()]);
            for complex in [false, true] {
                let (kind, flags) = if complex {
                    ("AC Analysis", "complex")
                } else {
                    ("Operating Point", "real")
                };
                let nvars = 3 * nl + usize::from(complex);
                raw.extend_from_slice(
                    format!(
                        "Title: * t\nDate: Thu Jan  1 00:00:00 1970\nPlotname: {kind} ({point})\n\
                         Flags: {flags}\nNo. Variables: {nvars}\nNo. Points: 1\nVariables:\n"
                    )
                    .as_bytes(),
                );
                let mut v = 0;
                if complex {
                    raw.extend_from_slice(b"\t0\tfrequency\tfrequency\n");
                    v = 1;
                }
                for n in 1..=nl {
                    for a in ["vda", "vga", "vdb"] {
                        raw.extend_from_slice(format!("\t{v}\ti({a}{n})\tcurrent\n").as_bytes());
                        v += 1;
                    }
                }
                raw.extend_from_slice(b"Binary:\n");
                let mut push = |x: f64| raw.extend_from_slice(&x.to_le_bytes());
                if complex {
                    push(F_AC);
                    push(0.0);
                }
                for n in 0..nl {
                    let [id, gm, gds, cgg] = f(k, n);
                    if complex {
                        let w = 2.0 * std::f64::consts::PI * F_AC;
                        for (re, im) in [(gm, -1e-9), (1e-12, cgg * w), (gds, 3e-9)] {
                            push(re);
                            push(im);
                        }
                    } else {
                        for x in [id, 1e-12, id] {
                            push(x);
                        }
                    }
                }
            }
        }
        raw
    }

    #[test]
    fn espice_raw_builds_the_lut_and_rejects_bad_files() {
        let s = parse(ESPICE).unwrap().remove(0).sweep.unwrap();
        let q = |k: usize, n: usize| {
            let x = (k + 1) as f64 * (n + 1) as f64;
            [1e-9 * x, 2e-8 * x, 3e-10 * x, 4e-16 * x]
        };
        let raw = espice_raw(&s, q);
        let blocks = parse_espice(&s, &raw).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].q[5], q(5, 1));
        let t = build("n45", &s, &blocks).unwrap();
        assert_eq!((t.l.len(), t.vds.len(), t.vgs.len()), (2, 12, 73));
        // Row k = d·73 + g; per metre of the 1 µm width.
        let k = 73 * 3 + 10;
        let close = |a: f32, b: f64| (f64::from(a) - b).abs() <= 1e-6 * b;
        assert!(close(t.id_w[t.idx(1, 3, 10)], q(k, 1)[0] / 1e-6));
        assert!(close(t.gm_w[t.idx(0, 3, 10)], q(k, 0)[1] / 1e-6));
        assert!(close(t.gds_w[t.idx(0, 3, 10)], q(k, 0)[2] / 1e-6));
        assert!(close(t.cgg_w[t.idx(1, 3, 10)], q(k, 1)[3] / 1e-6));
        assert!(
            t.provenance.starts_with("espice characterization") && t.provenance.contains("overlap")
        );

        // Negative space.
        assert!(
            parse_espice(&s, &raw[..raw.len() - 1]).is_err(),
            "truncated"
        );
        let mut extra = raw.clone();
        extra.extend_from_slice(&raw[..600]);
        assert!(
            parse_espice(&s, &extra)
                .unwrap_err()
                .contains("past the planned grid")
        );
        let mut one_l = s.clone();
        one_l.l_um = vec![0.05, 0.1, 1.0];
        assert!(
            parse_espice(&one_l, &raw).unwrap_err().contains("i(vda3)"),
            "missing ammeter"
        );
        let mut p = s.clone();
        p.polarity = Polarity::P;
        assert!(
            parse_espice(&p, &raw)
                .unwrap_err()
                .contains("off the planned grid")
        );
        // The first `.ac` plot names another point than its `.op` plot.
        let find = |from: usize, pat: &[u8]| {
            from + raw[from..]
                .windows(pat.len())
                .position(|w| w == pat)
                .unwrap()
        };
        let mut swapped = raw.clone();
        swapped[find(find(0, b"Plotname: AC"), b"vgs=") + 4] = b'9';
        assert!(
            parse_espice(&s, &swapped)
                .unwrap_err()
                .contains("not an .op/.ac pair")
        );
        // A later point whose header differs (another variable count) is refused.
        let mut grown = raw.clone();
        let second = find(find(0, b"Binary:") + 8, b"No. Variables: 6");
        grown[second + 15] = b'7';
        let at = find(second, b"Variables:\n") + 11;
        grown.splice(at..at, b"\t9\tv(x)\tvoltage\n".iter().copied());
        assert!(
            parse_espice(&s, &grown)
                .unwrap_err()
                .contains("header differs")
        );
    }
}
