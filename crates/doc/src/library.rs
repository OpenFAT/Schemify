//! Project manifest, lockfile and library index (D10, D35; netlist spec §1).
//!
//! Six questions (project-level tables):
//! 1. In: `schemify.project` and `schemify.lock` bytes, and per library a listing of
//!    `(relpath, bytes)` the app read (native fs, OPFS, a fetched git checkout or url
//!    archive). Out: cell rows with interfaces, resolution of `lib:cell` / `./cell`,
//!    content hashes, lockfile text.
//! 2. Libraries ≤ 2^8 (row 0 is the project itself; the builtin `core` comes last
//!    unless the manifest declares `core`); cells ≤ 2^16 per project (ARCHITECTURE);
//!    lock rows ≤ 2^8 + plugins.
//! 3. Library index `u8`; names, dirs and stems are `Sym`s in the project arena;
//!    hashes are 32-byte digests.
//! 4. Resolution scans `cells` by (lib, dir, stem) — a linear scan over ≤ 2^16 rows per
//!    lookup; `ponytail:` add a sorted index if netlisting 10^5-instance designs shows it.
//! 5. Lifetime: the project; a library's rows are replaced when it is re-indexed.
//! 6. Indexing is per library and independent; the app may index on worker threads
//!    and call in sequence.

use crate::document::Document;
use crate::record::RecordBuf;
use crate::sha256::{Digest, Sha256, hex};
use crate::strings::{Strings, Sym};
use crate::{Code, Diag};
use schemify_format::{Document as FormatDoc, FileKind, RecordKind, parse_prefix};

/// Where a library comes from (manifest `lib` record; `core` is builtin).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibSource {
    /// The project directory itself (library row 0, not referable by `lib:`).
    Project,
    /// Bundled with the tool (`lib core builtin`, implicit).
    Builtin,
    /// `lib <name> path <relpath>`: a directory relative to the project.
    Path {
        /// Project-relative directory.
        path: String,
    },
    /// `lib <name> git <url> <rev>`: the app clones and checks out `rev`.
    Git {
        /// Repository url.
        url: String,
        /// Branch, tag or commit as written; the lockfile pins the resolved commit.
        rev: String,
    },
    /// `lib <name> url <url> <sha256>`: the app downloads and verifies the archive hash
    /// (use [`crate::sha256`]) before listing it.
    Url {
        /// Archive url.
        url: String,
        /// Expected archive hash as written in the manifest.
        sha256: String,
    },
}

impl LibSource {
    /// Returns the lockfile `kind` word.
    pub fn word(&self) -> &'static str {
        match self {
            LibSource::Project => "project",
            LibSource::Builtin => "builtin",
            LibSource::Path { .. } => "path",
            LibSource::Git { .. } => "git",
            LibSource::Url { .. } => "url",
        }
    }
}

/// Library rows (manifest order after the two implicit rows).
#[derive(Clone, Debug, Default)]
pub struct Libs {
    /// Library name (`""` for the project row).
    pub name: Vec<String>,
    /// Source.
    pub source: Vec<LibSource>,
    /// `subdir <relpath>` inside a git/url source, applied by the app when listing.
    pub subdir: Vec<Option<String>>,
    /// Content hash of the last indexed listing; `None` until indexed.
    pub hash: Vec<Option<Digest>>,
    /// What the app resolved the source to when indexing (commit, url, path).
    pub resolved: Vec<Option<String>>,
}

/// Most libraries in one project.
pub const LIBS_MAX: usize = 1 << 8;
/// Most indexed cells in one project.
pub const CELLS_MAX: usize = 1 << 16;

impl Libs {
    /// Returns the row of library `name`.
    pub fn find(&self, name: &str) -> Option<usize> {
        let row = self.name.iter().position(|n| n == name);
        assert!(row.is_none_or(|r| r < self.name.len()));
        row
    }

    fn push(&mut self, name: &str, source: LibSource, subdir: Option<String>) {
        assert!(
            self.name.len() < LIBS_MAX,
            "a project has at most 2^8 libraries"
        );
        self.name.push(name.to_owned());
        self.source.push(source);
        self.subdir.push(subdir);
        self.hash.push(None);
        self.resolved.push(None);
        assert_eq!(self.name.len(), self.resolved.len());
    }
}

/// Lockfile rows (`locked <type> <name> <kind> <resolved> <sha256>`), sorted by
/// (type, name) when written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Locked {
    /// `lib` or `plugin`.
    pub ty: Vec<String>,
    /// Library or plugin name.
    pub name: Vec<String>,
    /// Source kind word (`path`, `git`, `url`).
    pub kind: Vec<String>,
    /// Resolved source (commit, url, path).
    pub resolved: Vec<String>,
    /// Content hash, lowercase hex.
    pub hash: Vec<String>,
}

impl Locked {
    /// Returns the row for (`ty`, `name`).
    pub fn find(&self, ty: &str, name: &str) -> Option<usize> {
        (0..self.ty.len()).find(|&i| self.ty[i] == ty && self.name[i] == name)
    }

    /// Sets (adds or replaces) a row.
    pub(crate) fn set(&mut self, ty: &str, name: &str, kind: &str, resolved: &str, hash: &str) {
        let row = match self.find(ty, name) {
            Some(r) => r,
            None => {
                self.ty.push(ty.to_owned());
                self.name.push(name.to_owned());
                self.kind.push(String::new());
                self.resolved.push(String::new());
                self.hash.push(String::new());
                self.ty.len() - 1
            }
        };
        self.kind[row] = kind.to_owned();
        self.resolved[row] = resolved.to_owned();
        self.hash[row] = hash.to_owned();
        assert_eq!(self.find(ty, name), Some(row));
    }

    /// Writes the lockfile in canonical form.
    pub fn write(&self) -> String {
        let mut d = FormatDoc::new(FileKind::Lock);
        for i in 0..self.ty.len() {
            d.begin_record(RecordKind::Locked);
            for f in [
                &self.ty[i],
                &self.name[i],
                &self.kind[i],
                &self.resolved[i],
                &self.hash[i],
            ] {
                d.push_field(f);
            }
        }
        let text = schemify_format::write(&d);
        assert!(text.starts_with("schemify 1 lock\n"));
        text
    }
}

/// How an indexed library's content compares with its lockfile row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockStatus {
    /// No lockfile row (call `Project::lock_library` to pin it).
    Unlocked,
    /// The content hash equals the locked hash.
    Match,
    /// The content changed since it was locked (a PDK update, a tampered fetch).
    Mismatch {
        /// The hash in the lockfile.
        locked: String,
    },
}

/// Indexed cell files: one row per `.shprim`/`.shcomp`/`.shbench` of every indexed
/// library. Row indices are valid until the next `index_library`.
#[derive(Clone, Debug, Default)]
pub struct Cells {
    /// Library row.
    pub lib: Vec<u8>,
    /// Directory inside the library (`""` at its root), in the project arena.
    pub dir: Vec<Sym>,
    /// Cell name (the file stem), in the project arena.
    pub stem: Vec<Sym>,
    /// File kind from the extension.
    pub kind: Vec<FileKind>,
    /// sha256 of the file bytes.
    pub hash: Vec<Digest>,
    /// The symbol (ranks 0–2, `parse_prefix`: header, interface and symbol graphics),
    /// for cells; `None` for benches and for files whose symbol does not parse (see the
    /// index report). Enough to place and draw an instance without opening the cell.
    pub iface: Vec<Option<Document>>,
}

impl Cells {
    fn retain_not_lib(&mut self, lib: u8) {
        fn retain_by<T>(v: &mut Vec<T>, keep: &[bool]) {
            let mut k = keep.iter();
            v.retain(|_| *k.next().expect("one flag per row"));
        }
        let keep: Vec<bool> = self.lib.iter().map(|&l| l != lib).collect();
        retain_by(&mut self.dir, &keep);
        retain_by(&mut self.stem, &keep);
        retain_by(&mut self.kind, &keep);
        retain_by(&mut self.hash, &keep);
        retain_by(&mut self.iface, &keep);
        retain_by(&mut self.lib, &keep);
        assert!(self.columns_ok() && !self.lib.contains(&lib));
    }

    fn columns_ok(&self) -> bool {
        let n = self.lib.len();
        [
            self.dir.len(),
            self.stem.len(),
            self.kind.len(),
            self.hash.len(),
            self.iface.len(),
        ]
        .iter()
        .all(|&l| l == n)
    }

    /// Returns the row of (`lib`, `dir`, `stem`) with kind `kind`.
    pub(crate) fn find(&self, lib: u8, dir: Sym, stem: Sym, kind: FileKind) -> Option<usize> {
        assert!(self.columns_ok());
        let row = (0..self.lib.len()).find(|&i| {
            self.lib[i] == lib && self.dir[i] == dir && self.stem[i] == stem && self.kind[i] == kind
        });
        assert!(row.is_none_or(|r| self.kind[r] == kind));
        row
    }
}

/// A parsed cell reference (netlist spec §1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellRef<'a> {
    /// `lib:cell`, resolved through the manifest.
    Lib {
        /// Library name.
        lib: &'a str,
        /// Cell name.
        cell: &'a str,
    },
    /// `./cell`, resolved in the referencing file's directory.
    Local {
        /// Cell name.
        cell: &'a str,
    },
}

impl<'a> CellRef<'a> {
    /// Parses `lib:cell` or `./cell`; `None` for anything else.
    pub fn parse(text: &'a str) -> Option<CellRef<'a>> {
        let ok = |s: &str| !s.is_empty() && !s.contains(['/', ':']);
        if let Some(cell) = text.strip_prefix("./") {
            return ok(cell).then_some(CellRef::Local { cell });
        }
        let (lib, cell) = text.split_once(':')?;
        let r = (ok(lib) && ok(cell)).then_some(CellRef::Lib { lib, cell });
        assert!(r.is_none() || lib.len() + cell.len() + 1 == text.len());
        r
    }
}

/// The manifest's non-library records, kept for the app and the plugin host.
#[derive(Clone, Debug, Default)]
pub(crate) struct ManifestRest {
    pub(crate) plugins: Vec<RecordBuf>,
    pub(crate) sets: Vec<(String, String)>,
}

/// Parses `schemify.project` into library rows (project and builtin `core` first).
pub(crate) fn parse_manifest(
    bytes: Option<&[u8]>,
) -> Result<(Libs, ManifestRest), schemify_format::ParseError> {
    let mut libs = Libs::default();
    libs.push("", LibSource::Project, None);
    let mut rest = ManifestRest::default();
    let Some(bytes) = bytes else {
        libs.push("core", LibSource::Builtin, None);
        return Ok((libs, rest));
    };
    let d = schemify_format::parse(bytes)?;
    if d.kind() != FileKind::Project {
        return Err(not_kind());
    }
    let mut core_declared = false;
    for r in 1..d.record_count() {
        let f: Vec<&str> = d.fields(r).collect();
        match d.record_kind(r) {
            RecordKind::Lib => {
                let source = match (f[1], f.get(3)) {
                    ("path", None) => LibSource::Path {
                        path: f[2].to_owned(),
                    },
                    ("git", Some(rev)) => LibSource::Git {
                        url: f[2].to_owned(),
                        rev: (*rev).to_owned(),
                    },
                    ("url", Some(h)) => LibSource::Url {
                        url: f[2].to_owned(),
                        sha256: (*h).to_owned(),
                    },
                    _ => return Err(not_kind()),
                };
                let subdir = d
                    .find_attr(r, "subdir")
                    .and_then(|a| d.attr_value(a, 0))
                    .map(str::to_owned);
                core_declared |= f[0] == "core";
                libs.push(f[0], source, subdir);
            }
            RecordKind::Plugin => rest.plugins.push(RecordBuf::from_format(&d, r)),
            RecordKind::Set => rest.sets.push((f[0].to_owned(), f[1].to_owned())),
            _ => {}
        }
    }
    if !core_declared {
        libs.push("core", LibSource::Builtin, None);
    }
    assert_eq!(libs.source[0], LibSource::Project);
    Ok((libs, rest))
}

/// Parses `schemify.lock`.
pub(crate) fn parse_lock(bytes: Option<&[u8]>) -> Result<Locked, schemify_format::ParseError> {
    let mut lock = Locked::default();
    let Some(bytes) = bytes else { return Ok(lock) };
    let d = schemify_format::parse(bytes)?;
    if d.kind() != FileKind::Lock {
        return Err(not_kind());
    }
    for r in 1..d.record_count() {
        let f: Vec<&str> = d.fields(r).collect();
        assert_eq!(f.len(), 5, "the reader checked the shape");
        lock.set(f[0], f[1], f[2], f[3], f[4]);
    }
    Ok(lock)
}

fn not_kind() -> schemify_format::ParseError {
    schemify_format::ParseError {
        diagnostics: vec![schemify_format::Diagnostic {
            code: schemify_format::Code::E028,
            line: 1,
            col: 1,
            record: schemify_format::NO_RECORD,
            detail: "wrong file kind or library source shape",
        }],
    }
}

/// Content hash of a library listing: sha256 over the files sorted bytewise by path,
/// each contributing `relpath`, a 0 byte and the sha256 of its bytes (`digests[i]` is
/// the sha256 of file `i`).
///
/// # Panics
///
/// Panics on a listing that names one path twice.
pub(crate) fn content_hash(files: &[(&str, &[u8])], digests: &[Digest]) -> Digest {
    assert_eq!(files.len(), digests.len());
    let mut sorted: Vec<(&str, &Digest)> = files.iter().map(|f| f.0).zip(digests).collect();
    sorted.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    assert!(
        sorted.windows(2).all(|w| w[0].0 != w[1].0),
        "a listing names each path once"
    );
    let mut h = Sha256::default();
    for (path, digest) in &sorted {
        h.update(path.as_bytes());
        h.update(&[0]);
        h.update(*digest);
    }
    h.finish()
}

/// Splits `a/b/c.shcomp` into (`a/b`, `c`, kind); `None` for non-cell files.
pub(crate) fn split_cell_path(relpath: &str) -> Option<(&str, &str, FileKind)> {
    let (dir, file) = relpath.rsplit_once('/').unwrap_or(("", relpath));
    let (stem, ext) = file.rsplit_once('.')?;
    let kind = match ext {
        "shprim" => FileKind::Shprim,
        "shcomp" => FileKind::Shcomp,
        "shbench" => FileKind::Shbench,
        _ => return None,
    };
    assert!(!stem.contains('/') && dir.len() < relpath.len());
    (!stem.is_empty()).then_some((dir, stem, kind))
}

/// Highest rank an indexed cell keeps: the symbol graphics (rank 2), so an editor draws
/// instances from the index instead of opening every instantiated cell.
const SYMBOL_RANK: u8 = 2;

/// Indexes one library's listing into `cells`, replacing its previous rows. Returns
/// per-file findings as (relpath, diag): `E020` same stem with two kinds in one
/// directory, interface parse errors (the format code).
pub(crate) fn index_cells(
    cells: &mut Cells,
    strings: &mut Strings,
    lib: u8,
    files: &[(&str, &[u8])],
    digests: &[Digest],
) -> Vec<(String, Diag)> {
    assert_eq!(files.len(), digests.len());
    cells.retain_not_lib(lib);
    let mut diags = Vec::new();
    for ((path, bytes), digest) in files.iter().zip(digests) {
        let Some((dir, stem, kind)) = split_cell_path(path) else {
            continue;
        };
        let (dir, stem) = (strings.intern(dir), strings.intern(stem));
        let twin = (0..cells.lib.len()).any(|i| {
            cells.lib[i] == lib
                && cells.dir[i] == dir
                && cells.stem[i] == stem
                && cells.kind[i] != kind
        });
        if twin {
            diags.push((
                (*path).to_owned(),
                Diag {
                    code: Code::E020,
                    line: 0,
                    detail: "same cell stem with two kinds",
                },
            ));
        }
        let iface = match kind {
            FileKind::Shbench => None,
            _ => match parse_prefix(bytes, SYMBOL_RANK) {
                Ok(model) => Some(Document::from_format(&model).doc),
                Err(e) => {
                    let d = e.diagnostics[0];
                    diags.push((
                        (*path).to_owned(),
                        Diag {
                            code: Code::from_format(d.code),
                            line: d.line,
                            detail: d.detail,
                        },
                    ));
                    None
                }
            },
        };
        assert!(
            cells.lib.len() < CELLS_MAX,
            "a project indexes at most 2^16 cells"
        );
        cells.lib.push(lib);
        cells.dir.push(dir);
        cells.stem.push(stem);
        cells.kind.push(kind);
        cells.hash.push(*digest);
        cells.iface.push(iface);
    }
    assert!(cells.columns_ok());
    diags
}

/// Compares a library's content hash with its lockfile row.
pub(crate) fn lock_status(lock: &Locked, name: &str, hash: &Digest) -> LockStatus {
    assert!(!name.is_empty(), "the project row is never locked");
    let status = match lock.find("lib", name) {
        None => LockStatus::Unlocked,
        Some(r) if lock.hash[r] == hex(hash) => LockStatus::Match,
        Some(r) => LockStatus::Mismatch {
            locked: lock.hash[r].clone(),
        },
    };
    assert!(!matches!(&status, LockStatus::Mismatch { locked } if *locked == hex(hash)));
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256::sha256;

    #[test]
    fn cell_refs_parse() {
        assert_eq!(
            CellRef::parse("sky130:nfet_01v8"),
            Some(CellRef::Lib {
                lib: "sky130",
                cell: "nfet_01v8"
            })
        );
        assert_eq!(
            CellRef::parse("./ota5t"),
            Some(CellRef::Local { cell: "ota5t" })
        );
        for bad in ["ota", "a:b:c", "./a/b", ":x", "x:", "./", "../x"] {
            assert_eq!(CellRef::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn content_hash_ignores_listing_order_but_not_names() {
        let a: &[(&str, &[u8])] = &[("a.shcomp", b"x"), ("b.shprim", b"y")];
        let b: &[(&str, &[u8])] = &[("b.shprim", b"y"), ("a.shcomp", b"x")];
        let c: &[(&str, &[u8])] = &[("a.shcomp", b"y"), ("b.shprim", b"x")];
        let h = |f: &[(&str, &[u8])]| {
            content_hash(f, &f.iter().map(|x| sha256(x.1)).collect::<Vec<_>>())
        };
        assert_eq!(h(a), h(b));
        assert_ne!(h(a), h(c));
    }

    #[test]
    fn paths_split_into_dir_stem_kind() {
        assert_eq!(
            split_cell_path("lib/ota5t.shcomp"),
            Some(("lib", "ota5t", FileKind::Shcomp))
        );
        assert_eq!(
            split_cell_path("x.shbench"),
            Some(("", "x", FileKind::Shbench))
        );
        assert_eq!(split_cell_path("README.md"), None);
        assert_eq!(split_cell_path(".shprim"), None);
    }
}
