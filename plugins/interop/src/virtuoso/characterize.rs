//! Characterization files (spec `interop.md` §8.3): what a PDK device's CDF callbacks derive
//! from the parameters a user sets, measured in Virtuoso by `SfyCharacterize`. Callbacks are
//! SKILL and never cross to Schemify; this file is the measured behaviour the PDK mirror's
//! rules are built and verified against. It names a PDK's parameters and values, so it stays
//! on the machine it was made on.

use crate::sfyx::{self, Record, Value};

/// Most points one device may have (`SfyCharacterize` writes at most 4024).
pub const POINTS_MAX: usize = 10_000;

/// One characterized device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    /// Library and cell.
    pub lib: String,
    /// See [`Device::lib`].
    pub cell: String,
    /// The inputs the sweep set, in the order it set them (`w`, `l`, then the finger count).
    pub inputs: Vec<String>,
    /// The netlisted parameters (spectre `instParameters`), in CDF order.
    pub outputs: Vec<String>,
    /// The points, in file order.
    pub points: Vec<Point>,
}

/// One measured point.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    /// The value set per input (`""`: not set at this point), parallel to [`Device::inputs`].
    pub set: Vec<String>,
    /// Whether every callback ran; else the reason, as SfySetParams gave it.
    pub status: Result<(), String>,
    /// The value read back per netlisted parameter, parallel to [`Device::outputs`].
    pub values: Vec<String>,
}

/// How each netlisted parameter behaved over a device's successful points.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Points whose callbacks all ran.
    pub ok: usize,
    /// Points where a callback failed.
    pub failed: usize,
    /// Parameters that are inputs themselves (they may still be rewritten by a callback).
    pub inputs: Vec<String>,
    /// Parameters that change with the inputs: the callback-derived ones.
    pub derived: Vec<String>,
    /// Parameters that kept one value.
    pub constant: Vec<String>,
}

fn strs(r: &Record, v: &[Value], what: &str) -> Result<Vec<String>, String> {
    v.iter()
        .map(|x| match x {
            Value::Str(s) => Ok(s.clone()),
            _ => Err(format!("line {}: {what}: not a string", r.line)),
        })
        .collect()
}

/// Reads a characterization file.
///
/// # Errors
///
/// A record that is not `char`/`pt` between the `sfyx` header and `end`, a `pt` before any
/// `char`, a `pt` whose lists do not match its `char`'s, or more than [`POINTS_MAX`]
/// points for one device.
pub fn read(text: &str) -> Result<Vec<Device>, String> {
    let recs = sfyx::read(text).map_err(|e| format!("line {}: {}", e.line, e.msg))?;
    let mut out: Vec<Device> = Vec::new();
    for r in &recs {
        match r.tag.as_str() {
            "sfyx" | "end" => {}
            "char" => {
                r.arity(4).map_err(|e| e.msg)?;
                out.push(Device {
                    lib: r.str(0).map_err(|e| e.msg)?.to_owned(),
                    cell: r.str(1).map_err(|e| e.msg)?.to_owned(),
                    inputs: strs(r, r.list(2).map_err(|e| e.msg)?, "inputs")?,
                    outputs: strs(r, r.list(3).map_err(|e| e.msg)?, "netlisted")?,
                    points: Vec::new(),
                });
            }
            "pt" => {
                r.arity(3).map_err(|e| e.msg)?;
                let d = out
                    .last_mut()
                    .ok_or_else(|| format!("line {}: a point before any `char`", r.line))?;
                let set = strs(r, r.list(0).map_err(|e| e.msg)?, "set")?;
                let values = strs(r, r.list(2).map_err(|e| e.msg)?, "values")?;
                if set.len() != d.inputs.len() || values.len() != d.outputs.len() {
                    return Err(format!(
                        "line {}: {} set and {} values for {} inputs and {} parameters",
                        r.line,
                        set.len(),
                        values.len(),
                        d.inputs.len(),
                        d.outputs.len()
                    ));
                }
                if d.points.len() == POINTS_MAX {
                    return Err(format!("line {}: more than {POINTS_MAX} points", r.line));
                }
                let status = match r.str(1).map_err(|e| e.msg)? {
                    "ok" => Ok(()),
                    why => Err(why.to_owned()),
                };
                d.points.push(Point {
                    set,
                    status,
                    values,
                });
            }
            t => return Err(format!("line {}: unknown record `{t}`", r.line)),
        }
    }
    debug_assert!(out.iter().all(|d| d.points.len() <= POINTS_MAX));
    Ok(out)
}

impl Device {
    /// Each netlisted parameter classified over the points whose callbacks ran.
    pub fn summary(&self) -> Summary {
        let ok: Vec<&Point> = self.points.iter().filter(|p| p.status.is_ok()).collect();
        let mut s = Summary {
            ok: ok.len(),
            failed: self.points.len() - ok.len(),
            ..Summary::default()
        };
        for (k, name) in self.outputs.iter().enumerate() {
            let first = ok.first().map(|p| &p.values[k]);
            if ok.iter().all(|p| Some(&p.values[k]) == first) {
                s.constant.push(name.clone());
            } else if self.inputs.contains(name) {
                s.inputs.push(name.clone());
            } else {
                s.derived.push(name.clone());
            }
        }
        debug_assert_eq!(
            s.inputs.len() + s.derived.len() + s.constant.len(),
            self.outputs.len()
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"("sfyx" 1 "schemify.il 1")
("char" "pdk" "nch" ("w" "nf") ("w" "nf" "ad" "m"))
("pt" ("1e-06" "1") "ok" ("1u" "1" "1f" "1"))
("pt" ("1e-06" "2") "ok" ("1u" "2" "0.5f" "1"))
("pt" ("4e-06" "1") "\"callback: w failed\"" ("4u" "1" "?" "1"))
("end")
"#;

    #[test]
    fn a_file_reads_and_classifies_its_parameters() {
        let d = &read(FILE).unwrap()[0];
        assert_eq!((d.lib.as_str(), d.cell.as_str()), ("pdk", "nch"));
        assert_eq!(d.points.len(), 3);
        assert!(d.points[2].status.is_err());
        let s = d.summary();
        assert_eq!((s.ok, s.failed), (2, 1));
        // The failed point's values are not evidence: w is constant over the ok points.
        assert_eq!(s.constant, ["w", "m"]);
        assert_eq!(s.inputs, ["nf"]);
        assert_eq!(s.derived, ["ad"]);
    }

    #[test]
    fn malformed_files_are_refused() {
        let bad = [
            FILE.replace(
                "(\"char\" \"pdk\" \"nch\" (\"w\" \"nf\") (\"w\" \"nf\" \"ad\" \"m\"))\n",
                "",
            ),
            FILE.replace(
                "(\"1e-06\" \"1\") \"ok\" (\"1u\" \"1\" \"1f\" \"1\")",
                "(\"1e-06\") \"ok\" (\"1u\" \"1\" \"1f\" \"1\")",
            ),
            FILE.replace("(\"end\")", "(\"what\")\n(\"end\")"),
        ];
        for b in &bad {
            assert!(read(b).is_err(), "{b}");
        }
    }
}
