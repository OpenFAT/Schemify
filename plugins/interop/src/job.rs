//! One conversion as a pure state machine (spec `interop.md` §1): the command line answers
//! its reads from the file system, the wasm guest through the host's inverted I/O, and both
//! get identical results because neither holds any logic of its own.
//!
//! ```text
//! Job::new(command) → loop { step() → Read(paths) → supply(path, bytes|None)…
//!                                   | Done { writes, summary } | Failed(problems) }
//! ```

use std::collections::BTreeSet;

use crate::design::{Files, LoadError, load};
use crate::map::Mapping;
use crate::notes::Notes;
use crate::virtuoso::{self, ExportOptions, ImportOptions};

/// What to convert. Paths are project-relative.
#[derive(Clone, Debug)]
pub enum Command {
    /// Schemify cells → one `.sfyx` file.
    VirtuosoExport {
        /// Cell files to export (with everything they instantiate).
        tops: Vec<String>,
        /// Output `.sfyx` path.
        out: String,
        /// Export settings.
        opts: ExportOptions,
    },
    /// A `.sfyx` file → Schemify files, merged with existing ones.
    VirtuosoImport {
        /// The dump to read.
        sfyx: String,
        /// Import settings.
        opts: ImportOptions,
    },
    /// Schemify cells → xschem `.sym`/`.sch` files under `out_dir`.
    XschemExport {
        /// Cell files to export.
        tops: Vec<String>,
        /// Output directory.
        out_dir: String,
    },
    /// Writes `schemify.il` (and the probe) into `dir`.
    InstallSkill {
        /// Output directory.
        dir: String,
    },
}

/// What the driver must do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Read these paths and [`Job::supply`] each (bytes, or `None` when absent).
    Read(Vec<String>),
    /// Write these files; the job is finished.
    Done {
        /// (project-relative path, bytes), sorted by path.
        writes: Vec<(String, Vec<u8>)>,
        /// One line for the log or terminal.
        summary: String,
    },
    /// The conversion cannot proceed; nothing is written.
    Failed(Vec<String>),
}

/// Rounds of reads a job may ask for (each round is one level of the reference closure).
pub const ROUNDS_MAX: u32 = 4096;

/// A conversion in progress.
pub struct Job {
    command: Command,
    files: Files,
    absent: BTreeSet<String>,
    asked: BTreeSet<String>,
    rounds: u32,
}

impl Job {
    /// Starts `command`.
    pub fn new(command: Command) -> Job {
        Job {
            command,
            files: Files::new(),
            absent: BTreeSet::new(),
            asked: BTreeSet::new(),
            rounds: 0,
        }
    }

    /// Hands in the result of one read asked for by [`Step::Read`].
    ///
    /// # Panics
    ///
    /// When `path` was not asked for (a driver bug).
    pub fn supply(&mut self, path: &str, bytes: Option<Vec<u8>>) {
        assert!(
            self.asked.remove(path),
            "supply of {path}, which was not asked for"
        );
        match bytes {
            Some(b) => {
                self.files.insert(path.to_owned(), b);
            }
            None => {
                self.absent.insert(path.to_owned());
            }
        }
    }

    /// Paths asked for and not yet supplied.
    pub fn pending(&self) -> usize {
        self.asked.len()
    }

    /// Advances the job. Call again after supplying every path of a [`Step::Read`].
    ///
    /// # Panics
    ///
    /// When reads are still pending.
    pub fn step(&mut self) -> Step {
        assert!(
            self.asked.is_empty(),
            "step with {} reads pending",
            self.asked.len()
        );
        self.rounds += 1;
        if self.rounds > ROUNDS_MAX {
            return Step::Failed(vec![format!("more than {ROUNDS_MAX} read rounds")]);
        }
        let command = self.command.clone();
        match command {
            Command::InstallSkill { dir } => {
                let dir = dir.trim_end_matches('/');
                let join = |f: &str| {
                    if dir.is_empty() {
                        f.to_owned()
                    } else {
                        format!("{dir}/{f}")
                    }
                };
                Step::Done {
                    writes: vec![
                        (
                            join("schemify.il"),
                            crate::skill::schemify_il().into_bytes(),
                        ),
                        (
                            join("sfy_probe.il"),
                            crate::skill::PROBE.as_bytes().to_vec(),
                        ),
                    ],
                    summary: format!(
                        "wrote {} and {}; in Virtuoso: (load \"<absolute path>/schemify.il\")",
                        join("schemify.il"),
                        join("sfy_probe.il")
                    ),
                }
            }
            Command::VirtuosoExport {
                tops,
                out,
                mut opts,
            } => {
                let map = match self.mapping() {
                    Ok(m) => m,
                    Err(s) => return s,
                };
                if let Some(p) = opts.target.clone().filter(|_| opts.target_map.is_none()) {
                    if !self.known(&p) {
                        return self.ask(vec![p]);
                    }
                    let parsed = match self.files.get(&p).map(|b| std::str::from_utf8(b)) {
                        None => return Step::Failed(vec![format!("{p}: cannot read")]),
                        Some(Err(_)) => return Step::Failed(vec![format!("{p}: not UTF-8")]),
                        Some(Ok(t)) => crate::retarget::parse_target(t),
                    };
                    match parsed {
                        Ok(t) => opts.target_map = Some(t),
                        Err(e) => {
                            return Step::Failed(
                                e.into_iter().map(|x| format!("{p}: {x}")).collect(),
                            );
                        }
                    }
                }
                let refs: Vec<&str> = tops.iter().map(String::as_str).collect();
                let d = match load(&self.files, &self.absent, &refs) {
                    Ok(d) => d,
                    Err(LoadError::Missing(p)) => return self.ask(p),
                    Err(LoadError::Invalid(p)) => return Step::Failed(p),
                };
                let top_ix: Vec<usize> = (0..tops.len()).collect();
                match virtuoso::export(&d, &top_ix, &map, &opts) {
                    Ok(e) => {
                        // OCEAN scripts beside the .sfyx, under `ocean/` (§9.6).
                        let dir = out.rsplit_once('/').map_or("", |(d, _)| d);
                        let mut writes = vec![(out.clone(), e.sfyx.into_bytes())];
                        for (p, t) in e.ocean {
                            let path = if dir.is_empty() {
                                format!("ocean/{p}")
                            } else {
                                format!("{dir}/ocean/{p}")
                            };
                            writes.push((path, t.into_bytes()));
                        }
                        finish(writes, &out, &e.notes, "Virtuoso export")
                    }
                    Err(p) => Step::Failed(p),
                }
            }
            Command::VirtuosoImport { sfyx, opts } => {
                let map = match self.mapping() {
                    Ok(m) => m,
                    Err(s) => return s,
                };
                let Some(text) = self.files.get(&sfyx) else {
                    if self.absent.contains(&sfyx) {
                        return Step::Failed(vec![format!("{sfyx}: cannot read")]);
                    }
                    return self.ask(vec![sfyx]);
                };
                let Ok(text) = String::from_utf8(text.clone()) else {
                    return Step::Failed(vec![format!("{sfyx}: not UTF-8")]);
                };
                let targets = match virtuoso::import_targets(&text) {
                    Ok(t) => t,
                    Err(p) => return Step::Failed(p),
                };
                let need: Vec<String> = targets.into_iter().filter(|p| !self.known(p)).collect();
                if !need.is_empty() {
                    return self.ask(need);
                }
                match virtuoso::import(&text, &self.files, &map, &opts) {
                    Ok(imp) => {
                        let mut writes = imp.files;
                        let base = sfyx.strip_suffix(".sfyx").unwrap_or(&sfyx).to_owned();
                        writes.push((
                            format!("{base}.report.md"),
                            imp.notes.markdown("Virtuoso import").into_bytes(),
                        ));
                        writes.push((format!("{base}.report.txt"), imp.notes.text().into_bytes()));
                        writes.sort_by(|a, b| a.0.cmp(&b.0));
                        let n = writes.len() - 2;
                        Step::Done {
                            writes,
                            summary: format!(
                                "Virtuoso import: {n} files, {} report rows ({base}.report.md)",
                                imp.notes.rows.len()
                            ),
                        }
                    }
                    Err(p) => Step::Failed(p),
                }
            }
            Command::XschemExport { tops, out_dir } => {
                let refs: Vec<&str> = tops.iter().map(String::as_str).collect();
                let d = match load(&self.files, &self.absent, &refs) {
                    Ok(d) => d,
                    Err(LoadError::Missing(p)) => return self.ask(p),
                    Err(LoadError::Invalid(p)) => return Step::Failed(p),
                };
                let top_ix: Vec<usize> = (0..tops.len()).collect();
                match crate::xschem_out::export(&d, &top_ix) {
                    Ok(x) => {
                        let dir = out_dir.trim_end_matches('/');
                        let writes = x
                            .files
                            .into_iter()
                            .map(|(p, b)| {
                                (
                                    if dir.is_empty() {
                                        p
                                    } else {
                                        format!("{dir}/{p}")
                                    },
                                    b,
                                )
                            })
                            .collect();
                        finish(
                            writes,
                            &format!("{dir}/xschem-export"),
                            &x.notes,
                            "xschem export",
                        )
                    }
                    Err(p) => Step::Failed(p),
                }
            }
        }
    }

    fn known(&self, p: &str) -> bool {
        self.files.contains_key(p) || self.absent.contains(p)
    }

    /// `interop.map`, read once (absent = identity mapping).
    fn mapping(&mut self) -> Result<Mapping, Step> {
        const MAP: &str = "interop.map";
        if !self.known(MAP) {
            return Err(self.ask(vec![MAP.to_owned()]));
        }
        match self.files.get(MAP) {
            None => Ok(Mapping::default()),
            Some(b) => match std::str::from_utf8(b) {
                Ok(t) => Mapping::parse(t).map_err(|p| {
                    Step::Failed(p.into_iter().map(|e| format!("{MAP}: {e}")).collect())
                }),
                Err(_) => Err(Step::Failed(vec![format!("{MAP}: not UTF-8")])),
            },
        }
    }

    fn ask(&mut self, paths: Vec<String>) -> Step {
        let fresh: Vec<String> = paths.into_iter().filter(|p| !self.known(p)).collect();
        assert!(
            !fresh.is_empty(),
            "asked for nothing new: the job would loop"
        );
        self.asked.extend(fresh.iter().cloned());
        Step::Read(fresh)
    }
}

/// Adds the report files beside `base` and builds the summary.
fn finish(mut writes: Vec<(String, Vec<u8>)>, base: &str, notes: &Notes, what: &str) -> Step {
    let base = base.strip_suffix(".sfyx").unwrap_or(base);
    let n = writes.len();
    writes.push((
        format!("{base}.report.md"),
        notes.markdown(what).into_bytes(),
    ));
    writes.push((format!("{base}.report.txt"), notes.text().into_bytes()));
    writes.sort_by(|a, b| a.0.cmp(&b.0));
    let summary = format!(
        "{what}: {n} files, {} report rows ({base}.report.md)",
        notes.rows.len()
    );
    Step::Done { writes, summary }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_skill_writes_both_files() {
        let mut j = Job::new(Command::InstallSkill {
            dir: "interop".into(),
        });
        match j.step() {
            Step::Done { writes, .. } => {
                let names: Vec<&str> = writes.iter().map(|w| w.0.as_str()).collect();
                assert_eq!(names, ["interop/schemify.il", "interop/sfy_probe.il"]);
            }
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn export_asks_for_the_map_and_the_closure() {
        let mut j = Job::new(Command::XschemExport {
            tops: vec!["a.shcomp".into()],
            out_dir: "out".into(),
        });
        let Step::Read(p) = j.step() else { panic!() };
        assert_eq!(p, ["a.shcomp", "schemify.project"]);
        for x in p {
            j.supply(&x, None);
        }
        assert!(matches!(j.step(), Step::Failed(_)));
    }
}
