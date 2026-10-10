//! Characterization: the `gmid.chars` project config, the ngspice sweep netlist generated from
//! it, the parser for ngspice's `wrdata` output, and the LUT built from that output.
//!
//! Pure text in, text or [`Lut`] out; the plugin glue does the I/O (fs_write the netlist,
//! spawn ngspice, fs_read the data), and `examples/characterize.rs` does the same natively.
//! Config syntax and the sweep are specified in the crate README.

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
    /// OP-parameter vector prefix template (`@m.x{n}.msky130_fd_pr__nfet_01v8`); the sweep
    /// saves `<prefix>[id]`, `[gm]`, `[gds]`, `[cgg]`.
    pub op: String,
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
    polarity: Option<Polarity>,
    vdd: Option<f64>,
    l: Option<Vec<f64>>,
    w: Option<f64>,
    note: Option<String>,
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
            "note" => self.note.replace(one(&mut v)?).is_some(),
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
            let any =
                self.inst.is_some() || self.op.is_some() || self.vdd.is_some() || self.l.is_some();
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
        let (inst, op) = (need(self.inst, "inst")?, need(self.op, "op")?);
        if !inst.contains("{n}") || !inst.contains("{l}") || !op.contains("{n}") {
            return Err("`inst` needs {n} and {l}, `op` needs {n}".to_owned());
        }
        let vdd = self.vdd.ok_or("characterization needs `vdd`")?;
        if vdd > 100.0 {
            return Err("`vdd` above 100 V".to_owned());
        }
        Ok(Some(Sweep {
            deck,
            inst,
            op,
            polarity: self.polarity.unwrap_or(Polarity::N),
            vdd,
            l_um: self.l.ok_or("characterization needs `l`")?,
            w_um: self.w.unwrap_or(1.0),
            note: self.note.unwrap_or_default(),
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

/// The ngspice batch netlist for `name`: every L as a parallel instance on shared nodes,
/// one nested DC sweep (VGS inner, VDS outer), and per instance a `wrdata` of its four OP
/// vectors into [`data_path`]. One file per L keeps each file's parse inside one plugin
/// event's fuel. Paths are relative to the project root (ngspice's cwd).
pub fn netlist(name: &str, s: &Sweep) -> String {
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

/// Instance `n`'s saved vectors: `<op>[id] <op>[gm] <op>[gds] <op>[cgg]`.
fn vectors(s: &Sweep, n: usize) -> String {
    let prefix = s.op.replace("{n}", &n.to_string());
    let v: Vec<String> = ["id", "gm", "gds", "cgg"]
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
        provenance: format!("ngspice characterization; deck {}; {}", s.deck, s.note),
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
            polarity: Polarity::N,
            vdd: 1.8,
            l_um: vec![0.5],
            w_um: 2.0,
            note: String::new(),
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
}
