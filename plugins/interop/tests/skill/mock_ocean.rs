//! Mock OCEAN functions (simulator setup, DC sweeps, results) for the retarget flow
//! (spec `interop.md` §10.3). Signatures from the *OCEAN Reference* (5.1.41): `simulator`,
//! `design`, `resultsDir`, `save`, `analysis`, `desVar`, `run`, `getData`, `value`.
//!
//! The "simulator" is [`Ocean::device`]: a closed-form transistor the test installs. A run
//! records the design variables; `getData "<op>:<q>" ?result 'dc` returns a waveform handle
//! `(sfyMockWave q vds l)` for `q` in `ids gm gds cgg`, and `value wave vgs` evaluates the
//! device there, at the netlist's 1 µm width. Voltages are taken as magnitudes (a p-channel
//! sweep is the n-channel model mirrored).
//!
//! ponytail: the mock does not read the netlist it was given (the device, width and node
//! names are fixed); a parser for it is only worth writing with a real Spectre golden.

use super::super::V;
use super::MockDb;

/// Width the retarget netlist instantiates the device at (`w=1u`).
pub const NETLIST_W: f64 = 1e-6;

/// A closed-form device: `(|vgs|, |vds|, l) -> [id, gm, gds, cgg]` per metre of width.
pub type Device = fn(f64, f64, f64) -> [f64; 4];

/// Simulation state.
#[derive(Default)]
pub struct Ocean {
    /// The simulated device; `None` = no simulator (`run` fails).
    pub device: Option<Device>,
    /// The session's simulator (`simulator`), `None` before one is chosen.
    pub simulator: Option<String>,
    /// Design variables (`desVar`), latest value per name.
    pub desvars: Vec<(String, f64)>,
    /// `(vds, l)` of the last successful `run`.
    pub last_run: Option<(f64, f64)>,
    /// Successful runs.
    pub runs: u32,
}

fn num(v: &V) -> Option<f64> {
    match v {
        V::Int(i) => Some(*i as f64),
        V::Float(f) => Some(*f),
        _ => None,
    }
}

/// Handles an OCEAN function; `None` when `name` is not one.
pub fn call(db: &mut MockDb, name: &str, args: &[V]) -> Option<Result<V, String>> {
    let o = &mut db.ocean;
    let r = match name {
        "simulator" => match args {
            [V::Sym(s)] => {
                o.simulator = Some(s.to_string());
                Ok(V::Sym(s.clone()))
            }
            _ => Err("simulator: (simulator 's_simulator)".into()),
        },
        "design" | "resultsDir" => match args {
            [V::Str(s)] if o.simulator.is_some() || name == "resultsDir" => Ok(V::Str(s.clone())),
            [V::Str(_)] => Ok(V::Nil), // no session: OCEAN prints an error and returns nil
            _ => Err(format!("{name}: (… t_path)")),
        },
        "save" => match args.first() {
            Some(V::Sym(_)) if args[1..].iter().all(|a| matches!(a, V::Str(_))) => Ok(V::T),
            _ => Err("save: (save 's_saveType t_name…)".into()),
        },
        "analysis" => match args.first() {
            Some(V::Sym(_)) if args.len() % 2 == 1 => Ok(V::T),
            _ => Err("analysis: (analysis 's_type ?option value…)".into()),
        },
        "desVar" => {
            if !args.len().is_multiple_of(2) {
                return Some(Err("desVar: name/value pairs".into()));
            }
            for p in args.chunks(2) {
                match (&p[0], num(&p[1])) {
                    (V::Str(n), Some(v)) => {
                        o.desvars.retain(|(k, _)| k != &**n);
                        o.desvars.push((n.to_string(), v));
                    }
                    _ => return Some(Err("desVar: (desVar t_name f_value …)".into())),
                }
            }
            Ok(V::T)
        }
        "run" => {
            let var = |k: &str| o.desvars.iter().find(|(n, _)| n == k).map(|(_, v)| *v);
            match (o.device, o.simulator.as_deref(), var("vds"), var("lch")) {
                (Some(_), Some("spectre"), Some(vds), Some(l)) if args.is_empty() => {
                    o.last_run = Some((vds.abs(), l));
                    o.runs += 1;
                    Ok(V::str("/mock/psf"))
                }
                _ => Ok(V::Nil),
            }
        }
        "getData" => match args {
            [V::Str(sig), V::Sym(k), V::Sym(res)] if &**k == "?result" && &**res == "dc" => {
                let q = sig.rsplit_once(':').map_or("", |(_, q)| q);
                match (o.last_run, ["ids", "gm", "gds", "cgg"].contains(&q)) {
                    (Some((vds, l)), true) => Ok(V::list(vec![
                        V::sym("sfyMockWave"),
                        V::str(q),
                        V::Float(vds),
                        V::Float(l),
                    ])),
                    _ => Ok(V::Nil),
                }
            }
            _ => Err("getData: (getData t_name ?result 'dc)".into()),
        },
        "value" => match args {
            [w, x] => {
                let it = w.items();
                match (it.first(), it.get(1).and_then(V::as_str), num(x)) {
                    (Some(V::Sym(tag)), Some(q), Some(vgs)) if &**tag == "sfyMockWave" => {
                        let (vds, l) = (num(&it[2]).unwrap(), num(&it[3]).unwrap());
                        let d = o.device.expect("a wave exists only after a run");
                        let k = ["ids", "gm", "gds", "cgg"]
                            .iter()
                            .position(|n| *n == q)
                            .unwrap();
                        Ok(V::Float(d(vgs.abs(), vds, l)[k] * NETLIST_W))
                    }
                    _ => Err("value: (value o_waveform g_x)".into()),
                }
            }
            _ => Err("value: (value o_waveform g_x)".into()),
        },
        _ => return None,
    };
    Some(r)
}
