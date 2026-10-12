//! The built-in analogLib ⇄ core device table and SPICE source text ⇄ waveform params
//! (spec `interop.md` §6.5, §6.6).
//!
//! The table is data: one [`Device`] row per core cell, read by the exporter (simInfo of
//! `schemify_core` cells, `--analoglib` re-mastering) and the importer (ngspice templates
//! of analogLib mirror cells, re-mastering back). No transform branches on a cell name;
//! it looks the row up.

use super::ael::number_to_ael;

/// One core cell and its analogLib counterpart.
#[derive(Clone, Copy, Debug)]
pub struct Device {
    /// Core cell name (`core:<core>`).
    pub core: &'static str,
    /// analogLib cells (several for sources: by waveform, [`WAVEFORMS`] order); empty when
    /// the cell is not mapped (`schemify_core` only).
    pub alib: &'static [&'static str],
    /// analogLib terminal names, by core pin order.
    pub alib_pins: &'static [&'static str],
    /// `(core param, analogLib param)`; core params not listed are not sent to analogLib.
    pub params: &'static [(&'static str, &'static str)],
    /// ngspice templates of the analogLib mirror cells, parallel to [`Device::alib`].
    pub ngspice: &'static [&'static str],
    /// Spectre master of the `schemify_core` cell (`{model}` = the `model` param); `""`:
    /// no spectre simInfo (not netlisted to spectre, reviewed).
    pub spectre: &'static str,
    /// `(spectre param, CDF param)` of the `schemify_core` cell's spectre simInfo, in
    /// `instParameters` order.
    pub spectre_params: &'static [(&'static str, &'static str)],
    /// The core `value` param is SPICE source text (§6.6): voltage (`false`) or current.
    pub source: Option<bool>,
}

const SRC: &[&str] = &["vdc", "vpulse", "vsin", "vpwl", "vexp"];
const ISRC: &[&str] = &["idc", "ipulse", "isin", "ipwl", "iexp"];
const PM: &[&str] = &["PLUS", "MINUS"];

/// Spectre names ← CDF names of the waveform params of `schemify_core` sources.
const V_SPECTRE: &[(&str, &str)] = &[
    ("dc", "vdc"),
    ("mag", "acm"),
    ("phase", "acp"),
    ("type", "srcType"),
    ("val0", "v1"),
    ("val1", "v2"),
    ("period", "per"),
    ("delay", "td"),
    ("rise", "tr"),
    ("fall", "tf"),
    ("width", "pw"),
    ("sinedc", "vo"),
    ("ampl", "va"),
    ("freq", "freq"),
    ("damp", "theta"),
    ("td1", "td1"),
    ("tau1", "tau1"),
    ("td2", "td2"),
    ("tau2", "tau2"),
];
const I_SPECTRE: &[(&str, &str)] = &[
    ("dc", "idc"),
    ("mag", "acm"),
    ("phase", "acp"),
    ("type", "srcType"),
    ("val0", "i1"),
    ("val1", "i2"),
    ("period", "per"),
    ("delay", "td"),
    ("rise", "tr"),
    ("fall", "tf"),
    ("width", "pw"),
    ("sinedc", "io"),
    ("ampl", "ia"),
    ("freq", "freq"),
    ("damp", "theta"),
    ("td1", "td1"),
    ("tau1", "tau1"),
    ("td2", "td2"),
    ("tau2", "tau2"),
];

/// The §6.5 table.
pub const DEVICES: &[Device] = &[
    Device {
        core: "res",
        alib: &["res"],
        alib_pins: PM,
        params: &[("value", "r"), ("m", "m")],
        ngspice: &["R{name} {PLUS} {MINUS} {r} m={m}"],
        spectre: "resistor",
        spectre_params: &[("r", "value"), ("m", "m")],
        source: None,
    },
    Device {
        core: "cap",
        alib: &["cap"],
        alib_pins: PM,
        params: &[("value", "c"), ("m", "m")],
        ngspice: &["C{name} {PLUS} {MINUS} {c} m={m}"],
        spectre: "capacitor",
        spectre_params: &[("c", "value"), ("m", "m")],
        source: None,
    },
    Device {
        core: "ind",
        alib: &["ind"],
        alib_pins: PM,
        params: &[("value", "l"), ("m", "m")],
        ngspice: &["L{name} {PLUS} {MINUS} {l} m={m}"],
        spectre: "inductor",
        spectre_params: &[("l", "value"), ("m", "m")],
        source: None,
    },
    Device {
        core: "vsource",
        alib: SRC,
        alib_pins: PM,
        params: &[],
        ngspice: &[
            "V{name} {PLUS} {MINUS} dc {vdc} ac {acm} {acp}",
            "V{name} {PLUS} {MINUS} pulse({v1} {v2} {td} {tr} {tf} {pw} {per})",
            "V{name} {PLUS} {MINUS} sin({vo} {va} {freq} {td} {theta})",
            "V{name} {PLUS} {MINUS} pwl({t1} {v1} {t2} {v2})",
            "V{name} {PLUS} {MINUS} exp({v1} {v2} {td1} {tau1} {td2} {tau2})",
        ],
        spectre: "vsource",
        spectre_params: V_SPECTRE,
        source: Some(false),
    },
    Device {
        core: "isource",
        alib: ISRC,
        alib_pins: PM,
        params: &[],
        ngspice: &[
            "I{name} {PLUS} {MINUS} dc {idc} ac {acm} {acp}",
            "I{name} {PLUS} {MINUS} pulse({i1} {i2} {td} {tr} {tf} {pw} {per})",
            "I{name} {PLUS} {MINUS} sin({io} {ia} {freq} {td} {theta})",
            "I{name} {PLUS} {MINUS} pwl({t1} {i1} {t2} {i2})",
            "I{name} {PLUS} {MINUS} exp({i1} {i2} {td1} {tau1} {td2} {tau2})",
        ],
        spectre: "isource",
        spectre_params: I_SPECTRE,
        source: Some(true),
    },
    Device {
        core: "vcvs",
        alib: &["vcvs"],
        alib_pins: &["PLUS", "MINUS", "NC+", "NC-"],
        params: &[("value", "egain")],
        ngspice: &["E{name} {pins} {egain}"],
        spectre: "vcvs",
        spectre_params: &[("gain", "value")],
        source: None,
    },
    Device {
        core: "vccs",
        alib: &["vccs"],
        alib_pins: &["PLUS", "MINUS", "NC+", "NC-"],
        params: &[("value", "ggain")],
        ngspice: &["G{name} {pins} {ggain}"],
        spectre: "vccs",
        spectre_params: &[("gm", "value")],
        source: None,
    },
    Device {
        core: "cccs",
        alib: &["cccs"],
        alib_pins: PM,
        params: &[("vnam", "vref"), ("value", "fgain")],
        ngspice: &["F{name} {PLUS} {MINUS} {vref} {fgain}"],
        spectre: "cccs",
        spectre_params: &[("gain", "value"), ("probe", "vnam")],
        source: None,
    },
    Device {
        core: "ccvs",
        alib: &["ccvs"],
        alib_pins: PM,
        params: &[("vnam", "vref"), ("value", "hgain")],
        ngspice: &["H{name} {PLUS} {MINUS} {vref} {hgain}"],
        spectre: "ccvs",
        spectre_params: &[("rm", "value"), ("probe", "vnam")],
        source: None,
    },
    Device {
        core: "diode",
        alib: &["diode"],
        alib_pins: PM,
        params: &[("model", "model"), ("area", "area")],
        ngspice: &["D{name} {PLUS} {MINUS} {model} area={area}"],
        spectre: "{model}",
        spectre_params: &[("area", "area")],
        source: None,
    },
    Device {
        core: "nmos4",
        alib: &["nmos4"],
        alib_pins: &["D", "G", "S", "B"],
        params: &[("model", "model"), ("w", "w"), ("l", "l"), ("m", "m")],
        ngspice: &["M{name} {D} {G} {S} {B} {model} w={w} l={l} m={m}"],
        spectre: "{model}",
        spectre_params: &[("w", "w"), ("l", "l"), ("m", "m")],
        source: None,
    },
    Device {
        core: "pmos4",
        alib: &["pmos4"],
        alib_pins: &["D", "G", "S", "B"],
        params: &[("model", "model"), ("w", "w"), ("l", "l"), ("m", "m")],
        ngspice: &["M{name} {D} {G} {S} {B} {model} w={w} l={l} m={m}"],
        spectre: "{model}",
        spectre_params: &[("w", "w"), ("l", "l"), ("m", "m")],
        source: None,
    },
    Device {
        core: "npn",
        alib: &["npn"],
        alib_pins: &["C", "B", "E"],
        params: &[("model", "model"), ("area", "area"), ("m", "m")],
        ngspice: &["Q{name} {C} {B} {E} {model} area={area} m={m}"],
        spectre: "{model}",
        spectre_params: &[("area", "area"), ("m", "m")],
        source: None,
    },
    Device {
        core: "pnp",
        alib: &["pnp"],
        alib_pins: &["C", "B", "E"],
        params: &[("model", "model"), ("area", "area"), ("m", "m")],
        ngspice: &["Q{name} {C} {B} {E} {model} area={area} m={m}"],
        spectre: "{model}",
        spectre_params: &[("area", "area"), ("m", "m")],
        source: None,
    },
    Device {
        core: "ammeter",
        alib: &["iprobe"],
        alib_pins: PM,
        params: &[],
        ngspice: &["V{name} {PLUS} {MINUS} 0"],
        spectre: "iprobe",
        spectre_params: &[],
        source: None,
    },
    Device {
        core: "tline",
        alib: &["tline"],
        alib_pins: &["IN+", "IN-", "OUT+", "OUT-"],
        params: &[("z0", "zo"), ("td", "td")],
        ngspice: &["T{name} {pins} z0={zo} td={td}"],
        spectre: "tline",
        spectre_params: &[("z0", "z0"), ("td", "td")],
        source: None,
    },
    Device {
        core: "parax_cap",
        alib: &["cap"],
        alib_pins: PM,
        params: &[("value", "c")],
        ngspice: &["C{name} {PLUS} {MINUS} {c} m={m}"],
        spectre: "capacitor",
        spectre_params: &[("c", "value"), ("m", "m")],
        source: None,
    },
    Device {
        core: "bsource",
        alib: &[],
        alib_pins: &[],
        params: &[],
        ngspice: &[],
        spectre: "bsource",
        spectre_params: &[("v", "func")],
        source: None,
    },
];

/// The row of core cell `name`.
pub fn by_core(name: &str) -> Option<&'static Device> {
    DEVICES.iter().find(|d| d.core == name)
}

/// The row and waveform index of analogLib cell `name` (the first row naming it: `cap`
/// is the `cap` row, not `parax_cap`).
pub fn by_alib(name: &str) -> Option<(&'static Device, usize)> {
    DEVICES
        .iter()
        .find_map(|d| d.alib.iter().position(|a| *a == name).map(|k| (d, k)))
}

/// analogLib and basic symbols that netlist as a global net (research cdf-pdk §4.1): an
/// instance of one becomes a global label on import.
pub const GLOBAL_SYMBOLS: &[&str] = &[
    "gnd", "gnda", "gndd", "vcc", "vcca", "vccd", "vdd", "vdda", "vddd", "vee", "veea", "veed",
    "vrefgnd", "vss", "vssa", "vssd",
];

/// True for a global symbol of `analogLib` or `basic`.
pub fn is_global_symbol(lib: &str, cell: &str) -> bool {
    (lib == "analogLib" || (lib == "basic" && matches!(cell, "gnd" | "vdd" | "vcc" | "vss")))
        && GLOBAL_SYMBOLS.contains(&cell)
}

// ---------------------------------------------------------------------------------------
// §6.6 SPICE source text ⇄ waveform params
// ---------------------------------------------------------------------------------------

/// Waveforms, in [`Device::alib`] order: `(SPICE keyword, Spectre type, voltage param
/// names, current param names)`; index 0 is DC (no keyword).
pub const WAVEFORMS: [(&str, &str, &[&str], &[&str]); 5] = [
    ("", "dc", &[], &[]),
    (
        "PULSE",
        "pulse",
        &["v1", "v2", "td", "tr", "tf", "pw", "per"],
        &["i1", "i2", "td", "tr", "tf", "pw", "per"],
    ),
    (
        "SIN",
        "sine",
        &["vo", "va", "freq", "td", "theta"],
        &["io", "ia", "freq", "td", "theta"],
    ),
    ("PWL", "pwl", &[], &[]),
    (
        "EXP",
        "exp",
        &["v1", "v2", "td1", "tau1", "td2", "tau2"],
        &["i1", "i2", "td1", "tau1", "td2", "tau2"],
    ),
];

/// Most `(t, v)` pairs of a PWL kept as params (`t1 v1 … t8 v8`).
pub const PWL_PAIRS_MAX: usize = 8;

/// A parsed source: waveform index into [`WAVEFORMS`] and the params, values as CDF/AEL
/// text, in a fixed order (`vdc`/`idc`, `acm`, `acp`, then the waveform's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Index into [`WAVEFORMS`] (0 = DC only).
    pub wave: usize,
    /// `(param, value)`.
    pub params: Vec<(String, String)>,
}

/// Parses SPICE independent-source text (`[DC] v`, `AC mag [phase]`, one of
/// `PULSE(...)`/`SIN(...)`/`PWL(...)`/`EXP(...)`; case-insensitive, commas or spaces) into
/// waveform params. `current` selects the i-source names.
///
/// `None` for text outside that grammar, a value that is not a number literal, or a PWL
/// with more than [`PWL_PAIRS_MAX`] pairs: the caller exports `vdc` = the whole text and
/// reviews it (`source`).
pub fn parse_source(text: &str, current: bool) -> Option<Source> {
    let dcname = if current { "idc" } else { "vdc" };
    let mut params: Vec<(String, String)> = Vec::new();
    let mut wave = 0;
    let mut k = 0;
    let num = number_to_ael;
    let mut have_dc = false;
    let mut have_ac = false;
    let toks: Vec<String> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .flat_map(|w| {
            // Split `PULSE(0` into `PULSE`, `(`, `0`.
            let mut out = Vec::new();
            let mut cur = String::new();
            for ch in w.chars() {
                if ch == '(' || ch == ')' {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    out.push(ch.to_string());
                } else {
                    cur.push(ch);
                }
            }
            if !cur.is_empty() {
                out.push(cur);
            }
            out
        })
        .collect();
    while k < toks.len() {
        let w = toks[k].to_ascii_uppercase();
        match w.as_str() {
            "DC" => {
                if have_dc {
                    return None;
                }
                let v = toks.get(k + 1)?;
                params.push((dcname.into(), num(v)?));
                have_dc = true;
                k += 2;
            }
            "AC" => {
                if have_ac {
                    return None;
                }
                let v = toks.get(k + 1)?;
                params.push(("acm".into(), num(v)?));
                k += 2;
                if let Some(p) = toks.get(k).and_then(|p| number_to_ael(p)) {
                    params.push(("acp".into(), p));
                    k += 1;
                }
                have_ac = true;
            }
            kw if WAVEFORMS.iter().skip(1).any(|w| w.0 == kw) => {
                if wave != 0 {
                    return None;
                }
                wave = WAVEFORMS.iter().position(|w| w.0 == kw).expect("matched");
                if toks.get(k + 1).map(String::as_str) != Some("(") {
                    return None;
                }
                let close = toks[k + 2..].iter().position(|t| t == ")")? + k + 2;
                let vals: Vec<String> = toks[k + 2..close]
                    .iter()
                    .map(|v| num(v))
                    .collect::<Option<_>>()?;
                if wave == 3 {
                    if !vals.len().is_multiple_of(2)
                        || vals.len() / 2 > PWL_PAIRS_MAX
                        || vals.is_empty()
                    {
                        return None;
                    }
                    params.push(("tvpairs".into(), (vals.len() / 2).to_string()));
                    let vn = if current { "i" } else { "v" };
                    for (p, pair) in vals.chunks(2).enumerate() {
                        params.push((format!("t{}", p + 1), pair[0].clone()));
                        params.push((format!("{vn}{}", p + 1), pair[1].clone()));
                    }
                } else {
                    let names = if current {
                        WAVEFORMS[wave].3
                    } else {
                        WAVEFORMS[wave].2
                    };
                    if vals.len() > names.len() {
                        return None;
                    }
                    for (n, v) in names.iter().zip(vals) {
                        params.push(((*n).into(), v));
                    }
                }
                k = close + 1;
            }
            _ if !have_dc && k == 0 => {
                params.push((dcname.into(), num(&toks[k])?));
                have_dc = true;
                k += 1;
            }
            _ => return None,
        }
    }
    assert!(wave < WAVEFORMS.len());
    Some(Source { wave, params })
}

/// Formats waveform params back as SPICE source text (`DC 1 AC 1 0 PULSE(0 1.8 0 10p 10p
/// 5n 10n)`): `get` returns each param's value already in Schemify number syntax. A
/// waveform's trailing unset params are omitted; an unset param inside is `0`.
pub fn format_source(
    wave: usize,
    current: bool,
    mut get: impl FnMut(&str) -> Option<String>,
) -> String {
    assert!(wave < WAVEFORMS.len(), "waveform index {wave}");
    let mut out = String::new();
    if let Some(v) = get(if current { "idc" } else { "vdc" }) {
        out.push_str("DC ");
        out.push_str(&v);
    }
    if let Some(m) = get("acm") {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str("AC ");
        out.push_str(&m);
        if let Some(p) = get("acp") {
            out.push(' ');
            out.push_str(&p);
        }
    }
    if wave != 0 {
        let vals: Vec<Option<String>> = if wave == 3 {
            let n: usize = get("tvpairs")
                .and_then(|t| t.parse().ok())
                .unwrap_or(0)
                .min(PWL_PAIRS_MAX);
            let vn = if current { "i" } else { "v" };
            (1..=n)
                .flat_map(|p| [get(&format!("t{p}")), get(&format!("{vn}{p}"))])
                .collect()
        } else {
            let names = if current {
                WAVEFORMS[wave].3
            } else {
                WAVEFORMS[wave].2
            };
            names.iter().map(|n| get(n)).collect()
        };
        let last = vals.iter().rposition(Option::is_some).map_or(0, |p| p + 1);
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(WAVEFORMS[wave].0);
        out.push('(');
        for (k, v) in vals[..last].iter().enumerate() {
            if k > 0 {
                out.push(' ');
            }
            out.push_str(v.as_deref().unwrap_or("0"));
        }
        out.push(')');
    }
    if out.is_empty() {
        out.push('0');
    }
    out
}

/// The CDF params of a `schemify_core` source cell: `(name, default)` for every waveform
/// param, `srcType` first. Defaults come from parsing the core default text.
pub fn source_cdf_params(current: bool) -> Vec<&'static str> {
    let mut v: Vec<&'static str> =
        vec!["srcType", if current { "idc" } else { "vdc" }, "acm", "acp"];
    for w in &WAVEFORMS[1..] {
        for n in if current { w.3 } else { w.2 } {
            if !v.contains(n) {
                v.push(n);
            }
        }
    }
    v.push("tvpairs");
    const T: [&str; PWL_PAIRS_MAX] = ["t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8"];
    const V: [&str; PWL_PAIRS_MAX] = ["v1", "v2", "v3", "v4", "v5", "v6", "v7", "v8"];
    const I: [&str; PWL_PAIRS_MAX] = ["i1", "i2", "i3", "i4", "i5", "i6", "i7", "i8"];
    for p in 0..PWL_PAIRS_MAX {
        for n in [T[p], if current { I[p] } else { V[p] }] {
            if !v.contains(&n) {
                v.push(n);
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_rows_are_consistent() {
        for d in DEVICES {
            assert_eq!(d.alib.len(), d.ngspice.len(), "{}", d.core);
            if !d.alib.is_empty() {
                let core = schemify_doc::core_library()
                    .iter()
                    .find(|(n, _)| *n == format!("{}.shprim", d.core))
                    .unwrap_or_else(|| panic!("core cell {}", d.core));
                let text = std::str::from_utf8(core.1).unwrap();
                let pins = text.lines().filter(|l| l.starts_with("pin ")).count();
                assert_eq!(pins, d.alib_pins.len(), "{}", d.core);
                for (c, _) in d.params {
                    assert!(text.contains(&format!("param {c} ")), "{} {c}", d.core);
                }
            }
        }
        assert_eq!(by_alib("cap").unwrap().0.core, "cap");
        assert_eq!(by_alib("vpulse").unwrap().1, 1);
        assert!(is_global_symbol("analogLib", "gnd") && !is_global_symbol("basic", "vssa"));
    }

    #[test]
    fn sources_round_trip() {
        for (text, wave, back) in [
            ("0", 0, "DC 0"),
            ("dc 1 ac 1", 0, "DC 1 AC 1"),
            (
                "DC 1 AC 1 0 PULSE(0 1.8 0 10p 10p 5n 10n)",
                1,
                "DC 1 AC 1 0 PULSE(0 1.8 0 10p 10p 5n 10n)",
            ),
            ("sin(0, 1, 1meg)", 2, "SIN(0 1 1meg)"),
            ("pwl(0 0 1n 1.8)", 3, "PWL(0 0 1n 1.8)"),
            ("EXP(0 1 1n 2n 3n 4n)", 4, "EXP(0 1 1n 2n 3n 4n)"),
        ] {
            let s = parse_source(text, false).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(s.wave, wave, "{text}");
            let get = |n: &str| {
                s.params
                    .iter()
                    .find(|p| p.0 == n)
                    .map(|p| p.1.replace('M', "meg"))
            };
            assert_eq!(format_source(s.wave, false, get), back, "{text}");
        }
        let i = parse_source("pulse(0 1m)", true).unwrap();
        assert_eq!(
            i.params,
            [("i1".into(), "0".into()), ("i2".into(), "1m".into())]
        );
        for bad in ["dc {v}", "PULSE 0 1", "foo", "dc 1 dc 2", "pwl(0)"] {
            assert!(parse_source(bad, false).is_none(), "{bad}");
        }
        assert!(source_cdf_params(false).contains(&"t8"));
    }
}
