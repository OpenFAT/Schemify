//! gm/Id sizing plugin (D20, D26): exports the pure expression functions `gmid.*` backed by
//! per-device lookup tables, loads those tables from the project, and characterizes devices
//! with ngspice. Ported from the UW-ASIC `gmid-lut` salvage (own code). Spec: `README.md`.
//!
//! - [`lut`] — the table, its `.glut` v2 file format, and the one query ([`lut::Lut::point_at`]);
//! - [`chars`] — the `gmid.chars` config, the sweep netlist, `wrdata` parsing, LUT building;
//! - this module — the function set ([`FUNCTIONS`], [`eval`]), the loaded tables
//!   ([`Tables`]), and on wasm32 the guest glue (events, I/O chain, `schemify_call`).

pub mod chars;
pub mod lut;
#[cfg(test)]
mod synth;

use std::fmt::Write as _;

use schemify_abi::{Code, text};

use lut::Lut;

/// Exported functions as `(name, arity)`; the index is the function's `fn_id` modulo
/// [`FN_STRIDE`]. Arguments: device name, gm/Id (1/V), L (m), for the sized ones Id (A), and
/// for the `_at` rows (index [`AT`] and up, row `AT + k` is row `k` plus the argument) |VDS|
/// (V) last. The rows without it answer at the table's nominal VDS (`vdd / 2`). The last row,
/// [`CLAMP_L`], is `gmid.l(dev, l)`: `l` clamped into the device table's L range.
pub const FUNCTIONS: [(&str, u8); 15] = [
    ("gmid.vgs", 3),
    ("gmid.jd", 3),
    ("gmid.w", 4),
    ("gmid.gm", 4),
    ("gmid.gds", 4),
    ("gmid.gain", 3),
    ("gmid.ft", 3),
    ("gmid.vgs_at", 4),
    ("gmid.jd_at", 4),
    ("gmid.w_at", 5),
    ("gmid.gm_at", 5),
    ("gmid.gds_at", 5),
    ("gmid.gain_at", 4),
    ("gmid.ft_at", 4),
    ("gmid.l", 2),
];

/// Index of the first function taking VDS as its last argument (see [`FUNCTIONS`]).
pub const AT: usize = 7;

/// Index of `gmid.l(dev, l)`: the nearest length the device's table covers, so a design asks
/// for an L and gets what the PDK can do (ASAP7 gates are fixed: its table has one L).
pub const CLAMP_L: usize = 14;

/// `fn_id = index + FN_STRIDE × generation`: the generation moves whenever the loaded tables
/// change, so the re-declared `functions` set differs and the host recomposes and re-evaluates
/// (identical re-declarations are ignored, plugin-abi §9).
pub const FN_STRIDE: u32 = 16;

/// The `contribute functions` body for table generation `generation`.
pub fn functions_text(generation: u32) -> String {
    let mut t = String::new();
    for (i, (name, arity)) in FUNCTIONS.iter().enumerate() {
        let id = generation.wrapping_mul(FN_STRIDE).wrapping_add(i as u32);
        writeln!(t, "function {name} {id} {arity}").expect("writing to a String");
    }
    t
}

/// Loaded tables keyed by their `gmid.chars` device name (SoA, ≤ [`lut::DEVICES_MAX`]).
#[derive(Clone, Debug, Default)]
pub struct Tables {
    /// Device names.
    pub names: Vec<String>,
    /// Their tables.
    pub luts: Vec<Lut>,
}

impl Tables {
    /// Adds or replaces `name`'s table.
    ///
    /// # Errors
    ///
    /// `E_SPACE` when [`lut::DEVICES_MAX`] other devices are loaded.
    pub fn insert(&mut self, name: &str, lut: Lut) -> Result<(), Code> {
        assert_eq!(self.names.len(), self.luts.len());
        match self.names.iter().position(|n| n == name) {
            Some(i) => self.luts[i] = lut,
            None if self.names.len() >= lut::DEVICES_MAX => return Err(Code::Space),
            None => {
                self.names.push(name.to_owned());
                self.luts.push(lut);
            }
        }
        Ok(())
    }

    /// The table of device `name`.
    pub fn get(&self, name: &str) -> Option<&Lut> {
        self.names
            .iter()
            .position(|n| n == name)
            .map(|i| &self.luts[i])
    }
}

/// Evaluates function `fn_id` (see [`FUNCTIONS`]) on line-record `args`, e.g.
/// `"nfet_01v8" 15 5e-7 2e-5` (`gmid.w`) or `"nfet_01v8" 15 5e-7 2e-5 1.2` (`gmid.w_at`).
/// Pure: reads only `tables`.
///
/// # Errors
///
/// `E_RANGE` for an unknown function, wrong arity or argument type, an unknown device, L or
/// VDS outside the table, an unreachable gm/Id, a non-positive Id, or a non-finite result.
pub fn eval(tables: &Tables, fn_id: u32, args: &str) -> Result<f64, Code> {
    let f = (fn_id % FN_STRIDE) as usize;
    let &(_, arity) = FUNCTIONS.get(f).ok_or(Code::Range)?;
    if f == CLAMP_L {
        let mut v = text::values(args);
        let dev = v.need().map_err(|_| Code::Range)?;
        let l = v.need_num::<f64>().map_err(|_| Code::Range)?;
        v.end().map_err(|_| Code::Range)?;
        let ls = &tables.get(&dev).ok_or(Code::Range)?.l;
        assert!(!ls.is_empty(), "a loaded table has an L axis");
        return if l.is_finite() && l > 0.0 {
            Ok(l.clamp(ls[0], ls[ls.len() - 1]))
        } else {
            Err(Code::Range)
        };
    }
    let at_vds = (AT..CLAMP_L).contains(&f);
    let sized = arity - u8::from(at_vds) == 4;
    let mut v = text::values(args);
    let dev = v.need().map_err(|_| Code::Range)?;
    let mut num = || -> Result<f64, Code> { v.need_num::<f64>().map_err(|_| Code::Range) };
    let (gm_id, l) = (num()?, num()?);
    let id = if sized { num()? } else { 1.0 };
    let vds = if at_vds { Some(num()?) } else { None };
    v.end().map_err(|_| Code::Range)?;
    if !(id.is_finite() && id > 0.0) {
        return Err(Code::Range);
    }
    let lut = tables.get(&dev).ok_or(Code::Range)?;
    let p = match vds {
        Some(vds) => lut.point_at(gm_id, l, vds),
        None => lut.point(gm_id, l),
    }
    .map_err(|_| Code::Range)?;
    let gain = p.gm_w / p.gds_w;
    let x = match f % AT {
        0 => p.vgs,
        1 => p.jd,
        2 => id / p.jd,
        3 => gm_id * id,
        4 => gm_id * id / gain,
        5 => gain,
        6 => p.gm_w / (2.0 * std::f64::consts::PI * p.cgg_w),
        _ => unreachable!("AT is 7"),
    };
    if x.is_finite() {
        Ok(x)
    } else {
        Err(Code::Range)
    }
}

#[cfg(target_arch = "wasm32")]
mod guest_glue {
    //! Events → I/O chains → tables. Every I/O completion is matched to the request that issued
    //! it by handle; at most [`PENDING_MAX`] requests are outstanding.
    //!
    //! Load / `gmid.reload`: read `gmid.chars` → read each device's LUT → insert, re-declare.
    //! `gmid.characterize [dev]`: write netlist → spawn ngspice → read the per-L wrdata files
    //! (all at once; each parses in its own event, inside its own fuel) → when the last arrives,
    //! build, insert, re-declare → write LUT. Each step waits for the previous one's `IoDone`,
    //! so nothing relies on the host executing requests in issue order.

    use std::cell::RefCell;

    use schemify_abi::guest::{self, Host};
    use schemify_abi::{Code, Event, Kind, Level, Out, text};

    use crate::chars::{self, Block, Device};
    use crate::{Tables, eval, functions_text, lut::Lut};

    guest::export!(on_event, call);

    const PENDING_MAX: usize = 256;

    const COMMANDS: &str = "command gmid.characterize \"gm/Id: characterize devices\"\n\
                            command gmid.reload \"gm/Id: reload lookup tables\"\n";
    const UI: &str = "node tools.gmid_characterize item\n  parent menu.tools\n  \
                      label \"gm/Id: characterize devices\"\n  command gmid.characterize\n\
                      node tools.gmid_reload item\n  parent menu.tools\n  \
                      label \"gm/Id: reload lookup tables\"\n  command gmid.reload\n";

    /// What a pending I/O handle is for.
    enum Io {
        Chars,
        Lut(String),
        Netlist(String),
        Spawn(String),
        /// Device, 1-based instance (L) number.
        Data(String, usize),
        Save(String),
    }

    #[derive(Default)]
    struct State {
        devices: Vec<Device>,
        tables: Tables,
        generation: u32,
        pending: Vec<(u32, Io)>,
        /// Characterizations collecting their per-L blocks: (device, block per L).
        partial: Vec<(String, Vec<Option<Block>>)>,
    }

    thread_local! {
        /// Plugin state; the guest is single-threaded and the cell outlives events.
        static STATE: RefCell<State> = RefCell::new(State::default());
    }

    fn on_event(ev: Event<'_>, host: &Host, out: &mut Out) {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            match ev {
                Event::Load => {
                    out.contribute(Kind::Functions, &functions_text(0));
                    out.contribute_commands(COMMANDS);
                    out.contribute_ui(UI);
                    issue(&mut s, out, host.fs_read(chars::CHARS_PATH), Io::Chars);
                }
                Event::Command {
                    name: "gmid.reload",
                    ..
                } => {
                    issue(&mut s, out, host.fs_read(chars::CHARS_PATH), Io::Chars);
                }
                Event::Command {
                    name: "gmid.characterize",
                    args,
                } => {
                    let only = text::values(args)
                        .next()
                        .and_then(Result::ok)
                        .map(|v| v.into_owned());
                    characterize(&mut s, host, out, only.as_deref());
                }
                Event::IoDone {
                    handle,
                    status,
                    bytes,
                } => {
                    let Some(at) = s.pending.iter().position(|p| p.0 == handle) else {
                        return;
                    };
                    let (_, io) = s.pending.swap_remove(at);
                    done(&mut s, host, out, io, status, bytes);
                }
                _ => {}
            }
        });
    }

    fn issue(s: &mut State, out: &mut Out, handle: Result<u32, Code>, io: Io) {
        match handle {
            Ok(h) if s.pending.len() < PENDING_MAX => s.pending.push((h, io)),
            Ok(_) => out.log_at(Level::Error, "gmid: too many pending requests"),
            Err(c) => out.log_at(
                Level::Error,
                &format!("gmid: I/O refused ({c}); check the plugin's grants"),
            ),
        }
    }

    fn characterize(s: &mut State, host: &Host, out: &mut Out, only: Option<&str>) {
        let todo: Vec<(String, String)> = s
            .devices
            .iter()
            .filter(|d| only.is_none_or(|o| o == d.name))
            .filter_map(|d| {
                d.sweep
                    .as_ref()
                    .map(|sw| (d.name.clone(), chars::netlist(&d.name, sw)))
            })
            .collect();
        if todo.is_empty() {
            out.log_at(
                Level::Warn,
                "gmid: no device with a `deck` in gmid.chars (run gmid.reload after editing it)",
            );
        }
        for (name, netlist) in todo {
            let h = host.fs_write(&chars::netlist_path(&name), netlist.as_bytes());
            issue(s, out, h, Io::Netlist(name));
        }
    }

    fn redeclare(s: &mut State, out: &mut Out) {
        s.generation = s.generation.wrapping_add(1);
        out.contribute(Kind::Functions, &functions_text(s.generation));
    }

    fn done(s: &mut State, host: &Host, out: &mut Out, io: Io, status: i32, bytes: &[u8]) {
        // Text payloads (config, wrdata) must be UTF-8; LUTs are binary and never converted.
        let text = || std::str::from_utf8(bytes).map_err(|_| "not UTF-8".to_owned());
        match io {
            Io::Chars if status < 0 => {
                out.log_at(Level::Info, "gmid: no gmid.chars in the project")
            }
            Io::Chars => match text().and_then(chars::parse) {
                Ok(devices) => {
                    for d in &devices {
                        issue(s, out, host.fs_read(&d.lut), Io::Lut(d.name.clone()));
                    }
                    s.devices = devices;
                }
                Err(e) => out.log_at(Level::Error, &format!("gmid.chars: {e}")),
            },
            Io::Lut(name) if status < 0 => {
                out.log_at(Level::Info, &format!("gmid: {name} has no LUT yet"));
            }
            Io::Lut(name) => match Lut::decode(bytes) {
                Ok(lut) => {
                    let summary = format!("gmid: {name} loaded ({})", lut.provenance);
                    insert(s, out, &name, lut, &summary);
                }
                Err(e) => out.log_at(Level::Error, &format!("gmid: {name}: {e}")),
            },
            Io::Netlist(name) if status < 0 => {
                out.log_at(
                    Level::Error,
                    &format!("gmid: {name}: cannot write the netlist"),
                );
            }
            Io::Netlist(name) => {
                let h = host.spawn(&["ngspice", "-b", &chars::netlist_path(&name)], b"");
                issue(s, out, h, Io::Spawn(name));
            }
            Io::Spawn(name) if status != 0 => {
                let tail = &bytes[bytes.len().saturating_sub(300)..];
                let tail = String::from_utf8_lossy(tail);
                out.log_at(
                    Level::Error,
                    &format!("gmid: {name}: ngspice exited {status}: {tail}"),
                );
            }
            Io::Spawn(name) => {
                let Some(nl) = sweep_of(s, &name).map(|sw| sw.l_um.len()) else {
                    return;
                };
                s.partial.retain(|p| p.0 != name);
                s.partial.push((name.clone(), vec![None; nl]));
                for n in 1..=nl {
                    let h = host.fs_read(&chars::data_path(&name, n));
                    issue(s, out, h, Io::Data(name.clone(), n));
                }
            }
            Io::Data(name, n) => {
                let Some(at) = s.partial.iter().position(|p| p.0 == name) else {
                    return;
                };
                let block = if status < 0 {
                    Err("ngspice wrote no data".to_owned())
                } else {
                    text().and_then(chars::parse_block)
                };
                match block {
                    Ok(b) if n >= 1 && n <= s.partial[at].1.len() => {
                        s.partial[at].1[n - 1] = Some(b)
                    }
                    Ok(_) => return,
                    Err(e) => {
                        s.partial.swap_remove(at);
                        out.log_at(Level::Error, &format!("gmid: {name}, length #{n}: {e}"));
                        return;
                    }
                }
                if s.partial[at].1.iter().any(Option::is_none) {
                    return; // more lengths to come
                }
                let (_, blocks) = s.partial.swap_remove(at);
                let blocks: Vec<Block> = blocks.into_iter().flatten().collect();
                let Some(d) = s.devices.iter().find(|d| d.name == name).cloned() else {
                    return;
                };
                let Some(sw) = d.sweep.as_ref() else { return };
                match chars::build(&name, sw, &blocks) {
                    Ok(lut) => {
                        let mut file = Vec::new();
                        lut.encode(&mut file);
                        let summary = format!(
                            "gmid: {name} characterized: {} L × {} VDS × {} VGS",
                            lut.l.len(),
                            lut.vds.len(),
                            lut.vgs.len()
                        );
                        insert(s, out, &name, lut, &summary);
                        issue(s, out, host.fs_write(&d.lut, &file), Io::Save(name));
                    }
                    Err(e) => out.log_at(Level::Error, &format!("gmid: {name}: {e}")),
                }
            }
            Io::Save(name) if status < 0 => {
                out.log_at(Level::Error, &format!("gmid: {name}: cannot write the LUT"));
            }
            Io::Save(name) => out.log(&format!("gmid: {name}: LUT saved")),
        }
    }

    fn sweep_of<'a>(s: &'a State, name: &str) -> Option<&'a chars::Sweep> {
        s.devices.iter().find(|d| d.name == name)?.sweep.as_ref()
    }

    /// Loads `lut` as device `name` and re-declares the functions so the host re-evaluates.
    fn insert(s: &mut State, out: &mut Out, name: &str, lut: Lut, summary: &str) {
        match s.tables.insert(name, lut) {
            Ok(()) => {
                redeclare(s, out);
                out.log(summary);
            }
            Err(c) => out.log_at(Level::Error, &format!("gmid: {name}: {c}")),
        }
    }

    fn call(fn_id: u32, args: &str, result: &mut String) -> Result<(), Code> {
        let x = STATE.with(|s| eval(&s.borrow().tables, fn_id, args))?;
        text::push_num(result, x);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{self, Ekv};

    fn tables() -> (Ekv, Tables) {
        let m = Ekv::default();
        let mut t = Tables::default();
        t.insert("nsynth", synth::lut(&m, "nsynth")).unwrap();
        (m, t)
    }

    fn call(t: &Tables, f: &str, args: &str) -> Result<f64, Code> {
        let i = FUNCTIONS.iter().position(|(n, _)| *n == f).unwrap() as u32;
        // Any generation must reach the same function.
        eval(t, i + 3 * FN_STRIDE, args)
    }

    fn rel(a: f64, b: f64) -> f64 {
        (a - b).abs() / b.abs()
    }

    #[test]
    fn l_clamps_into_the_table_and_refuses_bad_input() {
        let (_, t) = tables();
        let ls = &t.get("nsynth").unwrap().l;
        let (lo, hi) = (ls[0], ls[ls.len() - 1]);
        let mid = (lo * hi).sqrt();
        assert_eq!(call(&t, "gmid.l", &format!("\"nsynth\" {mid}")), Ok(mid));
        assert_eq!(
            call(&t, "gmid.l", &format!("\"nsynth\" {}", lo / 10.0)),
            Ok(lo)
        );
        assert_eq!(
            call(&t, "gmid.l", &format!("\"nsynth\" {}", hi * 10.0)),
            Ok(hi)
        );
        for bad in [
            "\"nsynth\" 0",
            "\"nsynth\" -1e-6",
            "\"nope\" 1e-6",
            "\"nsynth\"",
            "\"nsynth\" 1e-6 2",
        ] {
            assert_eq!(call(&t, "gmid.l", bad), Err(Code::Range), "{bad}");
        }
    }

    /// LUT answers vs the analytic model solved exactly, at grid and off-grid L, across weak,
    /// moderate and strong inversion: every quantity within 2 % (VGS within 2 mV).
    #[test]
    fn lut_matches_the_analytic_model() {
        let (m, t) = tables();
        let ls = &t.luts[0].l;
        let mut worst = 0.0f64;
        let mut lengths: Vec<f64> = ls.clone();
        lengths.extend(ls.windows(2).map(|w| (w[0] * w[1]).sqrt()));
        lengths.extend(ls.windows(2).map(|w| 0.8 * w[0] + 0.2 * w[1]));
        for &l in &lengths {
            for g in [
                3.0, 5.0, 8.0, 10.0, 12.0, 15.0, 18.0, 20.0, 22.0, 25.0, 27.0,
            ] {
                let vgs = m.vgs_for(g);
                let q = m.quantities(vgs, 0.9, l);
                let args = format!("\"nsynth\" {g} {l} 2e-5");
                let (gain, ft) = (q[1] / q[2], q[1] / (2.0 * std::f64::consts::PI * q[3]));
                assert!(
                    (call(&t, "gmid.vgs", &args[..args.len() - 5]).unwrap() - vgs).abs() < 2e-3
                );
                for (f, want) in [("gmid.jd", q[0]), ("gmid.gain", gain), ("gmid.ft", ft)] {
                    let got = call(&t, f, &args[..args.len() - 5]).unwrap();
                    worst = worst.max(rel(got, want));
                    assert!(rel(got, want) < 0.02, "{f} g={g} l={l}: {got} vs {want}");
                }
                for (f, want) in [
                    ("gmid.w", 2e-5 / q[0]),
                    ("gmid.gds", 2e-5 * g / gain),
                    ("gmid.gm", g * 2e-5),
                ] {
                    let got = call(&t, f, &args).unwrap();
                    assert!(rel(got, want) < 0.02, "{f} g={g} l={l}: {got} vs {want}");
                }
            }
        }
        assert!(worst < 0.02, "worst relative error {worst}");
        eprintln!("worst relative error vs the analytic model: {worst:.5}");
    }

    #[test]
    fn monotone_in_gm_id_and_length() {
        let (_, t) = tables();
        let ls = t.luts[0].l.clone();
        let gs: Vec<f64> = (0..40).map(|k| 3.0 + 0.6 * f64::from(k)).collect();
        for &l in &ls {
            let at = |f: &str, g: f64| call(&t, f, &format!("\"nsynth\" {g} {l}")).unwrap();
            for w in gs.windows(2) {
                assert!(
                    at("gmid.jd", w[1]) < at("gmid.jd", w[0]),
                    "jd falls with gm/Id"
                );
                assert!(
                    at("gmid.ft", w[1]) < at("gmid.ft", w[0]),
                    "ft falls with gm/Id"
                );
                assert!(
                    at("gmid.vgs", w[1]) < at("gmid.vgs", w[0]),
                    "vgs falls with gm/Id"
                );
                let wd = |g: f64| call(&t, "gmid.w", &format!("\"nsynth\" {g} {l} 1e-5")).unwrap();
                assert!(wd(w[1]) > wd(w[0]), "w grows with gm/Id at fixed Id");
            }
        }
        for &g in &gs {
            let at = |f: &str, l: f64| call(&t, f, &format!("\"nsynth\" {g} {l}")).unwrap();
            let fine: Vec<f64> = (0..60)
                .map(|k| ls[0] * (ls[ls.len() - 1] / ls[0]).powf(f64::from(k) / 59.0))
                .collect();
            for w in fine.windows(2) {
                assert!(
                    at("gmid.gain", w[1]) > at("gmid.gain", w[0]),
                    "gain grows with L"
                );
                assert!(at("gmid.ft", w[1]) < at("gmid.ft", w[0]), "ft falls with L");
            }
        }
    }

    /// The `_at` functions: at the nominal VDS they equal the plain ones bit for bit; off it
    /// they follow the analytic model; without the VDS argument, or with a VDS off the table's
    /// axis, they fail.
    #[test]
    fn at_functions_take_vds_last() {
        let (m, t) = tables();
        let l = t.luts[0].l[5];
        for (plain, at, extra) in [
            ("gmid.vgs", "gmid.vgs_at", ""),
            ("gmid.jd", "gmid.jd_at", ""),
            ("gmid.w", "gmid.w_at", " 2e-5"),
            ("gmid.gm", "gmid.gm_at", " 2e-5"),
            ("gmid.gds", "gmid.gds_at", " 2e-5"),
            ("gmid.gain", "gmid.gain_at", ""),
            ("gmid.ft", "gmid.ft_at", ""),
        ] {
            let args = format!("\"nsynth\" 12 {l}{extra}");
            let want = call(&t, plain, &args);
            assert!(want.is_ok(), "{plain}({args})");
            assert_eq!(call(&t, at, &format!("{args} 0.9")), want, "{at} at VDD/2");
            assert_eq!(call(&t, at, &args), Err(Code::Range), "{at} without VDS");
            let off = format!("{args} 1.9");
            assert_eq!(
                call(&t, at, &off),
                Err(Code::Range),
                "{at} VDS off the axis"
            );
        }
        let vgs = m.vgs_for(12.0);
        for vds in [0.3, 0.5, 1.35] {
            let q = m.quantities(vgs, vds, l);
            let args = format!("\"nsynth\" 12 {l} {vds}");
            let jd = call(&t, "gmid.jd_at", &args).unwrap();
            assert!(rel(jd, q[0]) < 0.02, "jd at {vds}: {jd} vs {}", q[0]);
            let gain = call(&t, "gmid.gain_at", &args).unwrap();
            assert!(rel(gain, q[1] / q[2]) < 0.02, "gain at {vds}: {gain}");
        }
    }

    #[test]
    fn bad_calls_are_range_errors() {
        let (_, t) = tables();
        for (f, args) in [
            ("gmid.jd", "\"nope\" 15 1e-6"),
            ("gmid.jd", "\"nsynth\" 15 1e-9"),
            ("gmid.jd", "\"nsynth\" 15 1e-3"),
            ("gmid.jd", "\"nsynth\" 99 1e-6"),
            ("gmid.jd", "\"nsynth\" 0 1e-6"),
            ("gmid.jd", "\"nsynth\" x 1e-6"),
            ("gmid.jd", "\"nsynth\" 15"),
            ("gmid.jd", "\"nsynth\" 15 1e-6 2e-5"),
            ("gmid.w", "\"nsynth\" 15 1e-6"),
            ("gmid.w", "\"nsynth\" 15 1e-6 -2e-5"),
            ("gmid.w", "\"nsynth\" 15 1e-6 inf"),
            ("gmid.jd", ""),
            ("gmid.jd", "\"unterminated"),
        ] {
            assert_eq!(call(&t, f, args), Err(Code::Range), "{f}({args})");
        }
        assert_eq!(
            eval(&t, 14, "\"nsynth\" 15 1e-6"),
            Err(Code::Range),
            "fn index 14 is free"
        );
        assert_eq!(
            eval(&Tables::default(), 1, "\"nsynth\" 15 1e-6"),
            Err(Code::Range)
        );
    }

    /// The committed sky130 samples (`examples/characterize.rs`) decode and look like silicon:
    /// gm/Id peaks in weak inversion, nfet jd at gm/Id 15 and L 0.5 µm of a few A/m, ft in GHz,
    /// and jd / ft falling with gm/Id, gain rising and ft falling with L in moderate inversion.
    #[test]
    fn sky130_samples_are_plausible() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/app/tests/fixtures/ota/gmid/"
        );
        let mut t = Tables::default();
        for dev in ["nfet_01v8", "pfet_01v8"] {
            let bytes = std::fs::read(format!("{dir}{dev}.glut")).unwrap();
            let lut = Lut::decode(&bytes).unwrap();
            assert!(
                lut.provenance.contains("sky130") && lut.l.len() == 8,
                "{}",
                lut.provenance
            );
            t.insert(dev, lut).unwrap();
        }
        let at = |f: &str, dev: &str, g: f64, l: f64| {
            call(&t, f, &format!("\"{dev}\" {g} {l}")).unwrap()
        };
        let jd = at("gmid.jd", "nfet_01v8", 15.0, 0.5e-6);
        assert!((1.0..10.0).contains(&jd), "nfet jd {jd}");
        let ft = at("gmid.ft", "nfet_01v8", 15.0, 0.5e-6);
        assert!((1e9..1e10).contains(&ft), "nfet ft {ft}");
        assert!(at("gmid.gain", "nfet_01v8", 15.0, 0.5e-6) > 30.0);
        // The pfet's short and long bins dip in moderate inversion (a local gm/Id peak of 15–17
        // at L 0.15, 0.18 and 8 µm); the scan crosses the dip, so every row reaches weak
        // inversion (> 20 1/V) and VGS keeps falling as gm/Id rises across it.
        let p0 = t.get("pfet_01v8").unwrap().l[0];
        let (v14, v20) = (
            at("gmid.vgs", "pfet_01v8", 14.0, p0),
            at("gmid.vgs", "pfet_01v8", 20.0, p0),
        );
        assert!(v20 < v14, "pfet L {p0}: VGS {v20} at 20 vs {v14} at 14");
        for dev in ["nfet_01v8", "pfet_01v8"] {
            let ls = t.get(dev).unwrap().l.clone();
            for (li, l) in ls.iter().enumerate() {
                let (lo, hi) = t.get(dev).unwrap().gm_id_range(li);
                assert!(
                    lo < 3.0 && hi > 20.0 && hi < 40.0,
                    "{dev} L {l}: {lo}..{hi}"
                );
            }
            for &l in &ls {
                for g in [4.0, 6.0, 8.0, 10.0, 12.0] {
                    assert!(
                        at("gmid.jd", dev, g + 2.0, l) < at("gmid.jd", dev, g, l),
                        "{dev} jd"
                    );
                    assert!(
                        at("gmid.ft", dev, g + 2.0, l) < at("gmid.ft", dev, g, l),
                        "{dev} ft"
                    );
                }
            }
            for w in ls.windows(2) {
                assert!(
                    at("gmid.ft", dev, 10.0, w[1]) < at("gmid.ft", dev, 10.0, w[0]),
                    "{dev} ft vs L"
                );
                assert!(
                    at("gmid.gain", dev, 10.0, w[1]) > at("gmid.gain", dev, 10.0, w[0]),
                    "{dev} gain vs L {w:?}"
                );
            }
        }
    }

    #[test]
    fn registry_text() {
        assert_eq!(
            functions_text(0).lines().next(),
            Some("function gmid.vgs 0 3")
        );
        assert!(functions_text(2).contains("function gmid.ft 38 3\n"));
        assert!(functions_text(2).contains("function gmid.w_at 41 5\n"));
        assert_eq!(functions_text(0).lines().count(), FUNCTIONS.len());
        for (k, (name, arity)) in FUNCTIONS[AT..CLAMP_L].iter().enumerate() {
            assert_eq!(*name, format!("{}_at", FUNCTIONS[k].0));
            assert_eq!(
                *arity,
                FUNCTIONS[k].1 + 1,
                "{name}: one more argument (VDS)"
            );
        }
        assert!(FUNCTIONS.len() as u32 <= FN_STRIDE);
    }
}
