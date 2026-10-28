//! The compiled-source build cache (run spec §5): a job's [`Source`]s in, their outputs
//! (`<stem>.osdi|.vvp|.va`) in the shared cache directory out, then copied into each job
//! directory with the files a copied Verilog-A source `` `include``s.
//!
//! Compilers (OpenVAF, Icarus) are child processes polled like simulators: one compiler
//! per stale output serves every job that needs it, outputs are fresh by mtime, and a
//! failed output fails every job of the run that needs it. ESPice's Verilog-A sources
//! ([`Tool::Copy`]) skip the compiler: the source itself is cached and copied, its
//! includes found once per run.
//!
//! Contents:
//! - crate-only `struct Cache`: `ensure`, `reap`, `failure`, `compiling`, `busy`,
//!   `kill_all`, `place`

use crate::prepare::{Origin, Source, Tool};
use crate::process::{self, Process};
use crate::runner::RunnerConfig;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Compile state of one run's sources. Dropping it kills its compilers.
#[derive(Default)]
pub(crate) struct Cache {
    /// Compiler children, at most one per output.
    compiling: Vec<Compiling>,
    /// Outputs whose compile failed this run.
    failed: Vec<Failed>,
    /// Per copied `file` source found this run, the files it includes.
    includes: Vec<Includes>,
}

/// An output whose compile failed this run.
struct Failed {
    out: String,
    /// The message every job needing `out` fails with.
    message: String,
}

/// The files a copied `file` source includes.
struct Includes {
    /// The source's output name.
    out: String,
    /// The source's directory.
    dir: PathBuf,
    /// The included files, relative to `dir`.
    files: Vec<String>,
}

/// Most files one copied Verilog-A source may `` `include`` (transitively).
const INCLUDES_MAX: usize = 256;
/// Deepest `` `include`` nesting of a copied Verilog-A source.
const INCLUDE_DEPTH_MAX: u32 = 16;
/// Headers VerA (ESPice's Verilog-A compiler) supplies itself, so a source may include
/// them without a copy beside it.
const STANDARD_HEADERS: [&str; 4] = [
    "disciplines.vams",
    "constants.vams",
    "discipline.h",
    "constants.h",
];

/// One compiler child writing `out` into the build cache.
struct Compiling {
    out: String,
    process: Process,
    /// `` `openvaf` on <src> ``, for messages.
    what: String,
}

impl Cache {
    /// Starts the compiler of `s` unless its output is fresh, already compiling, or
    /// failed this run; a compiler that cannot start fails the output. A copied `file`
    /// first has its includes found (once per run); a missing one fails the output.
    pub(crate) fn ensure(&mut self, config: &RunnerConfig, s: &Source) {
        assert!(
            !s.out.is_empty(),
            "`sources` names every output `<stem>.<ext>`"
        );
        if self.failed.iter().any(|f| f.out == s.out)
            || self.compiling.iter().any(|c| c.out == s.out)
        {
            return;
        }
        if let (Tool::Copy, Origin::File { lib, path }) = (s.tool, &s.origin)
            && !self.includes.iter().any(|i| i.out == s.out)
        {
            let found = source_file(config, *lib, path).and_then(|src| {
                let dir = src.parent().map_or_else(PathBuf::new, Path::to_path_buf);
                va_includes(&src).map(|inc| (dir, inc))
            });
            match found {
                Ok((dir, files)) => self.includes.push(Includes {
                    out: s.out.clone(),
                    dir,
                    files,
                }),
                Err(message) => {
                    return self.failed.push(Failed {
                        out: s.out.clone(),
                        message,
                    });
                }
            }
        }
        match spawn_compile(config, s) {
            Ok(None) => {}
            Ok(Some(c)) => self.compiling.push(c),
            Err(message) => self.failed.push(Failed {
                out: s.out.clone(),
                message,
            }),
        }
        assert!(self.compiling.len() <= crate::JOBS_MAX as usize);
    }

    /// Reaps exited compilers; one that failed, wrote nothing or ran past the timeout
    /// fails its output.
    pub(crate) fn reap(&mut self, config: &RunnerConfig) {
        let mut i = 0;
        while i < self.compiling.len() {
            let Some(outcome) = self.compiling[i].process.reap(config.timeout, "compiler") else {
                i += 1;
                continue;
            };
            let c = self.compiling.remove(i);
            let wrote = config.cache.join(&c.out).exists();
            let why = match outcome {
                Ok(status) if status.success() && wrote => continue,
                Ok(status) => {
                    let log = config.cache.join(format!("{}.log", c.out));
                    let last = process::last_line(&log).unwrap_or_else(|| "no output".to_owned());
                    format!("{status}: {last}")
                }
                Err(msg) => msg,
            };
            self.failed.push(Failed {
                message: format!("{} failed ({why})", c.what),
                out: c.out,
            });
        }
        assert!(
            self.compiling
                .iter()
                .all(|c| !self.failed.iter().any(|f| f.out == c.out)),
            "an output compiles or failed, never both"
        );
    }

    /// The message of the first of `sources` whose output failed, if any.
    pub(crate) fn failure(&self, sources: &[Source]) -> Option<&str> {
        let f = sources
            .iter()
            .find_map(|s| self.failed.iter().find(|f| f.out == s.out));
        f.map(|f| f.message.as_str())
    }

    /// Whether any of `sources` is still compiling.
    pub(crate) fn compiling(&self, sources: &[Source]) -> bool {
        sources
            .iter()
            .any(|s| self.compiling.iter().any(|c| c.out == s.out))
    }

    /// Whether any compiler runs.
    pub(crate) fn busy(&self) -> bool {
        !self.compiling.is_empty()
    }

    /// Kills every compiler.
    pub(crate) fn kill_all(&mut self) {
        self.compiling.clear();
        assert!(!self.busy());
    }

    /// Copies the outputs of `sources` (and a copied source's includes, at their paths
    /// relative to it) into job directory `dir`.
    ///
    /// # Errors
    ///
    /// A copy or directory creation that fails, or two sources that include one relative
    /// path from different files (they cannot share the job directory).
    pub(crate) fn place(
        &self,
        config: &RunnerConfig,
        sources: &[Source],
        dir: &Path,
    ) -> Result<(), String> {
        assert!(
            self.failure(sources).is_none(),
            "a job with a failed source never starts"
        );
        assert!(
            !self.compiling(sources),
            "a job starts once its sources compiled"
        );
        // Include paths placed so far, with the file each came from.
        let mut placed: Vec<(&str, PathBuf)> = Vec::new();
        for s in sources {
            fs::copy(config.cache.join(&s.out), dir.join(&s.out))
                .map_err(|e| format!("copy {}: {e}", s.out))?;
            let Some(includes) = self.includes.iter().find(|i| i.out == s.out) else {
                continue;
            };
            for rel in &includes.files {
                let from = includes.dir.join(rel);
                match placed.iter().find(|p| p.0 == rel) {
                    Some((_, other)) if *other == from => continue,
                    Some((_, other)) => {
                        return Err(format!(
                            "`include \"{rel}\" names two files: {} and {}",
                            other.display(),
                            from.display()
                        ));
                    }
                    None => {}
                }
                let to = dir.join(rel);
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("create {}: {e}", parent.display()))?;
                }
                fs::copy(&from, &to).map_err(|e| format!("copy {}: {e}", from.display()))?;
                placed.push((rel, from));
            }
        }
        assert!(placed.len() <= INCLUDES_MAX * sources.len());
        Ok(())
    }
}

// ---- private ----

/// Starts compiling `s` into the build cache, or returns `None` when the cached output is
/// at least as new as the source. A `body` source is first written to the cache as
/// `<stem>.va|.v` (only when its text changed, so the mtime check holds across runs).
/// The compiler's output goes to `<cache>/<out>.log`. A [`Tool::Copy`] source never
/// starts a process: a `body` is its own output, a `file` is copied over a stale one.
fn spawn_compile(config: &RunnerConfig, s: &Source) -> Result<Option<Compiling>, String> {
    let cache = |e: io::Error| format!("cache {}: {e}", config.cache.display());
    let src = match &s.origin {
        Origin::File { lib, path } => source_file(config, *lib, path)?,
        Origin::Body(text) => {
            fs::create_dir_all(&config.cache).map_err(cache)?;
            let stem = s.out.rsplit_once('.').map_or(s.out.as_str(), |(st, _)| st);
            let ext = match s.tool {
                Tool::Openvaf | Tool::Copy => "va",
                Tool::Iverilog => "v",
            };
            let p = config.cache.join(format!("{stem}.{ext}"));
            if fs::read(&p).ok().as_deref() != Some(text.as_bytes()) {
                fs::write(&p, text).map_err(cache)?;
            }
            p
        }
    };
    let cached = config.cache.join(&s.out);
    let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified());
    let src_time = mtime(&src).map_err(|e| format!("source {}: {e}", src.display()))?;
    if mtime(&cached).is_ok_and(|t| t >= src_time) {
        return Ok(None);
    }
    fs::create_dir_all(&config.cache).map_err(cache)?;
    // A stale output must not pass for this compile's result if the compiler writes none.
    match fs::remove_file(&cached) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(cache(e)),
        _ => {}
    }
    let mut cmd;
    let (prog, what) = match s.tool {
        Tool::Openvaf => {
            cmd = Command::new(&config.tools.openvaf);
            cmd.arg(&src).arg("-o").arg(&cached);
            (&config.tools.openvaf, "Verilog-A → OSDI")
        }
        Tool::Iverilog => {
            cmd = Command::new(&config.tools.iverilog);
            cmd.arg("-o").arg(&cached).arg(&src);
            (&config.tools.iverilog, "Verilog → VVP")
        }
        Tool::Copy => {
            // A body was written as the output itself, which is never stale (above).
            assert_ne!(src, cached, "a copied body is its own output");
            fs::copy(&src, &cached)
                .map_err(|e| format!("copy {} to {}: {e}", src.display(), cached.display()))?;
            return Ok(None);
        }
    };
    let log = File::create(config.cache.join(format!("{}.log", s.out))).map_err(cache)?;
    let log2 = log.try_clone().map_err(cache)?;
    let process = Process::spawn(&mut cmd, log, log2).map_err(|e| {
        let need = format!("needed to compile {} ({what})", src.display());
        format!(
            "{}; {need}",
            process::spawn_error(prog, "compiler", "the user config", &e)
        )
    })?;
    let c = Compiling {
        out: s.out.clone(),
        process,
        what: format!("`{prog}` on {}", src.display()),
    };
    assert!(!c.out.is_empty());
    Ok(Some(c))
}

/// The on-disk path of a `file` source: `path` under library row `lib`'s directory.
fn source_file(config: &RunnerConfig, lib: u8, path: &str) -> Result<PathBuf, String> {
    let dir = config
        .lib_dirs
        .get(usize::from(lib))
        .ok_or_else(|| format!("no directory for library row {lib}"))?;
    Ok(dir.join(path))
}

/// Returns the files the Verilog-A source `src` includes, transitively, as paths relative
/// to its directory, sorted: each `` `include "<name>"`` resolves against the directory of
/// the file that names it (as ESPice's compiler does), so the copies keep their layout
/// beside the copied source (run spec §5). An absolute name is left to resolve by itself;
/// a missing [`STANDARD_HEADERS`] name is the compiler's own. Each file is read once, so
/// include cycles end.
///
/// Errors: a file that cannot be read, a missing include (naming it and the file that
/// includes it), a name that leaves the source's directory (`..` above it), more than
/// [`INCLUDES_MAX`] files, or nesting deeper than [`INCLUDE_DEPTH_MAX`].
fn va_includes(src: &Path) -> Result<Vec<String>, String> {
    let dir = src.parent().unwrap_or(Path::new(""));
    let mut out: Vec<String> = Vec::new();
    // (including file relative to `dir`, `None` for `src` itself; its depth)
    let mut stack: Vec<(Option<String>, u32)> = vec![(None, 0)];
    while let Some((file, depth)) = stack.pop() {
        assert!(depth <= INCLUDE_DEPTH_MAX);
        let path = file
            .as_ref()
            .map_or_else(|| src.to_path_buf(), |f| dir.join(f));
        let text =
            fs::read_to_string(&path).map_err(|e| format!("source {}: {e}", path.display()))?;
        let base = file
            .as_deref()
            .and_then(|f| f.rsplit_once('/'))
            .map(|p| p.0);
        for name in include_names(&text) {
            if Path::new(name).is_absolute() {
                continue;
            }
            let joined = base.map_or_else(|| name.to_owned(), |b| format!("{b}/{name}"));
            let rel = normalize(&joined).ok_or_else(|| {
                format!(
                    "{} includes \"{name}\", outside the source's directory (unsupported: \
                     the copy beside the job's netlist cannot reach it)",
                    path.display()
                )
            })?;
            if out.contains(&rel) {
                continue;
            }
            if !dir.join(&rel).is_file() {
                if STANDARD_HEADERS.contains(&name) {
                    continue;
                }
                return Err(format!(
                    "{} includes \"{name}\": no file {}",
                    path.display(),
                    dir.join(&rel).display()
                ));
            }
            if depth + 1 > INCLUDE_DEPTH_MAX || out.len() >= INCLUDES_MAX {
                return Err(format!(
                    "{}: includes nest deeper than {INCLUDE_DEPTH_MAX} or name more than \
                     {INCLUDES_MAX} files",
                    src.display()
                ));
            }
            out.push(rel.clone());
            stack.push((Some(rel), depth + 1));
        }
    }
    out.sort_unstable();
    assert!(out.len() <= INCLUDES_MAX);
    assert!(out.windows(2).all(|w| w[0] < w[1]), "each file once");
    Ok(out)
}

/// The quoted names of a Verilog-A text's `` `include`` directives, outside `//` comments.
// ponytail: `/* */` comments and `ifdef`-disabled branches are not skipped, so an include
// there must exist too; handle them when a model needs it.
fn include_names(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter_map(|line| {
        let code = line.split("//").next().unwrap_or("");
        let after = &code[code.find("`include")? + "`include".len()..];
        let name = &after[after.find('"')? + 1..];
        Some(&name[..name.find('"')?])
    })
}

/// `p` with `.` segments dropped and `..` applied, `/`-separated; `None` when it climbs
/// above its start or names nothing.
fn normalize(p: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    let out = (!parts.is_empty()).then(|| parts.join("/"));
    debug_assert!(
        out.as_deref()
            .is_none_or(|o| o.split('/').all(|seg| !matches!(seg, "" | "." | ".."))),
        "a relative path of plain segments"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn include_names_skip_line_comments() {
        let text =
            "`include \"a.vams\"\n// `include \"b.h\"\nx; `include \"c/d.h\" // e\n`include e.h\n";
        let names: Vec<&str> = include_names(text).collect();
        assert_eq!(names, ["a.vams", "c/d.h"]);
    }

    #[test]
    fn normalize_keeps_paths_inside_their_start() {
        assert_eq!(normalize("a/./b/../c").as_deref(), Some("a/c"));
        assert_eq!(normalize("a//b").as_deref(), Some("a/b"));
        assert_eq!(normalize("../a"), None);
        assert_eq!(normalize("a/../.."), None);
        assert_eq!(normalize("a/.."), None, "names nothing");
    }
}
