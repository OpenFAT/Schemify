//! `vfs`: the web build's file system table (D31, web spec §2). OPFS is asynchronous on the
//! main thread while every spine reads and writes synchronously, so the page holds every
//! OPFS file here: loaded once before the editor starts, read and written synchronously,
//! and written back to OPFS by one asynchronous flusher that drains [`Vfs::next_stale`].
//!
//! Pure data, no browser calls: the native tests run it against `std::fs` (the oracle).
//!
//! Data design (six questions):
//! 1. In: the OPFS tree at mount, then the platform calls (write, append, remove, read, list).
//!    Out: bytes, listings, and the queue of paths whose OPFS copy is stale.
//! 2. How many: ≤ [`FILES_MAX`] files, ≤ [`BYTES_MAX`] bytes in total; the queue holds each
//!    path once.
//! 3. Widths: paths are absolute `/`-separated strings; mtimes u64 seconds since the epoch.
//! 4. Access: by path (reads, writes); listings walk one directory's sorted children; the
//!    flusher pops the queue in the order the last change of each path happened.
//! 5. Lifetime: the page.
//! 6. Not parallel: the browser's main thread.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::path::{Component, Path, PathBuf};

use super::{LIST_DEPTH_MAX, LIST_FILES_MAX};

/// Files held at once (the mount stops there).
pub(crate) const FILES_MAX: usize = 1 << 18;
/// Bytes held at once: every OPFS file lives in memory too.
// ponytail: the whole OPFS tree is mirrored in RAM (≈ the bytes a native session reads
// anyway, plus other projects). Upgrade: mount the opened project, `/config` and `/state`
// only, and page in other trees before `Session::load`.
pub(crate) const BYTES_MAX: usize = 1 << 30;
/// Bytes one project import may bring in.
pub(crate) const IMPORT_BYTES_MAX: usize = 512 << 20;
/// Where imported projects go (web spec §3).
pub(crate) const PROJECTS: &str = "/projects";

/// The table. Directories are explicit (OPFS has empty directories); `/` always exists.
#[derive(Debug)]
pub(crate) struct Vfs {
    /// Bytes and modification time (s) per file path.
    files: BTreeMap<String, (Vec<u8>, u64)>,
    /// Directory paths.
    dirs: BTreeSet<String>,
    /// Total bytes in `files`.
    bytes: usize,
    /// Stale paths by change sequence number (the flush order) and the reverse index, so a
    /// path changed again moves to the back in O(log n).
    stale: BTreeMap<u64, String>,
    stale_at: BTreeMap<String, u64>,
    seq: u64,
}

/// The table key of `path`: absolute, `/`-separated, without `.` or `..` (a relative path
/// is taken from `/`, the page's working directory).
pub(crate) fn key(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    let k = format!("/{}", parts.join("/"));
    debug_assert!(k == "/" || !k.ends_with('/'));
    k
}

/// The parent directory key of `key`; `None` for `/`.
fn parent(key: &str) -> Option<&str> {
    match key.rsplit_once('/')? {
        (_, "") => None,
        ("", _) => Some("/"),
        (p, _) => Some(p),
    }
}

/// `dir/name` for a directory key.
fn child(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

impl Vfs {
    /// An empty file system: only `/`.
    pub(crate) fn new() -> Vfs {
        Vfs {
            files: BTreeMap::new(),
            dirs: BTreeSet::from(["/".to_owned()]),
            bytes: 0,
            stale: BTreeMap::new(),
            stale_at: BTreeMap::new(),
            seq: 0,
        }
    }

    /// A file's bytes.
    pub(crate) fn read(&self, key: &str) -> Option<&[u8]> {
        self.files.get(key).map(|f| f.0.as_slice())
    }

    pub(crate) fn is_dir(&self, key: &str) -> bool {
        self.dirs.contains(key)
    }

    pub(crate) fn exists(&self, key: &str) -> bool {
        self.files.contains_key(key) || self.dirs.contains(key)
    }

    pub(crate) fn mtime(&self, key: &str) -> Option<u64> {
        self.files.get(key).map(|f| f.1)
    }

    /// Creates directory `key` and its ancestors (not queued: OPFS directories are created
    /// with the first file written into them).
    ///
    /// # Errors
    ///
    /// `key` or an ancestor is a file.
    pub(crate) fn mkdirs(&mut self, key: &str) -> Result<(), String> {
        let mut at = Some(key);
        let mut todo = Vec::new();
        while let Some(k) = at {
            if self.files.contains_key(k) {
                return Err(format!("{k}: not a directory"));
            }
            if self.dirs.contains(k) {
                break;
            }
            todo.push(k.to_owned());
            at = parent(k);
        }
        self.dirs.extend(todo);
        assert!(self.dirs.contains(key));
        Ok(())
    }

    /// Replaces (or creates) file `key`, creating its directories; queues it for OPFS.
    ///
    /// # Errors
    ///
    /// `key` is a directory, an ancestor is a file, or the table would pass [`BYTES_MAX`].
    pub(crate) fn write(&mut self, key: &str, bytes: Vec<u8>, now_s: u64) -> Result<(), String> {
        self.put(key, bytes, now_s)?;
        self.mark(key);
        Ok(())
    }

    /// Appends to file `key` (created when missing); queues it for OPFS.
    ///
    /// # Errors
    ///
    /// As [`Vfs::write`].
    pub(crate) fn append(&mut self, key: &str, bytes: &[u8], now_s: u64) -> Result<(), String> {
        let mut all = self.read(key).map(<[u8]>::to_vec).unwrap_or_default();
        all.extend_from_slice(bytes);
        self.write(key, all, now_s)
    }

    /// Deletes file `key`; queues it for OPFS.
    ///
    /// # Errors
    ///
    /// No such file (a directory is not removed).
    pub(crate) fn remove(&mut self, key: &str) -> Result<(), String> {
        let (bytes, _) = self
            .files
            .remove(key)
            .ok_or_else(|| format!("{key}: no such file"))?;
        self.bytes -= bytes.len();
        self.mark(key);
        Ok(())
    }

    /// Adds a file read from OPFS at mount: not queued (OPFS already has it).
    ///
    /// # Errors
    ///
    /// As [`Vfs::write`], or [`FILES_MAX`] files are held already.
    pub(crate) fn load(&mut self, key: &str, bytes: Vec<u8>, mtime_s: u64) -> Result<(), String> {
        if self.files.len() >= FILES_MAX {
            return Err(format!("more than {FILES_MAX} files"));
        }
        self.put(key, bytes, mtime_s)
    }

    fn put(&mut self, key: &str, bytes: Vec<u8>, mtime_s: u64) -> Result<(), String> {
        if self.dirs.contains(key) {
            return Err(format!("{key}: is a directory"));
        }
        let dir = parent(key).ok_or("/: is a directory")?;
        let old = self.files.get(key).map_or(0, |f| f.0.len());
        let after = self.bytes - old + bytes.len();
        if after > BYTES_MAX {
            return Err(format!("{key}: web storage full ({} MiB)", BYTES_MAX >> 20));
        }
        self.mkdirs(dir)?;
        self.bytes = after;
        self.files.insert(key.to_owned(), (bytes, mtime_s));
        Ok(())
    }

    fn mark(&mut self, key: &str) {
        if let Some(old) = self.stale_at.insert(key.to_owned(), self.seq) {
            self.stale.remove(&old);
        }
        self.stale.insert(self.seq, key.to_owned());
        self.seq += 1;
        assert_eq!(self.stale.len(), self.stale_at.len());
    }

    /// The next path whose OPFS copy is stale, in the order of each path's last change (a
    /// save that writes a cell and then drops its journal reaches OPFS in that order).
    /// The flusher writes the path's current bytes, or removes it when it has none.
    pub(crate) fn next_stale(&mut self) -> Option<String> {
        let (_, key) = self.stale.pop_first()?;
        self.stale_at.remove(&key);
        Some(key)
    }

    pub(crate) fn has_stale(&self) -> bool {
        !self.stale.is_empty()
    }

    /// Lists files under `dir` like the native `platform::list`: each directory's children
    /// in name order, hidden entries (`.x`) and `target` skipped, `skip` subtrees skipped,
    /// at most [`LIST_FILES_MAX`] files and [`LIST_DEPTH_MAX`] levels.
    ///
    /// Cost: O(descendants) per directory visited.
    pub(crate) fn list(&self, dir: &str, depth: u32, skip: &[String], out: &mut Vec<PathBuf>) {
        if !self.dirs.contains(dir) {
            return;
        }
        let prefix = if dir == "/" {
            "/".to_owned()
        } else {
            format!("{dir}/")
        };
        let under = |k: &&String| k.starts_with(prefix.as_str());
        let range = (Bound::Included(prefix.as_str()), Bound::Unbounded);
        let files = self.files.range::<str, _>(range).map(|(k, _)| k);
        let dirs = self.dirs.range::<str, _>(range);
        let mut names: BTreeSet<&str> = BTreeSet::new();
        for k in files.take_while(under).chain(dirs.take_while(under)) {
            let rest = &k[prefix.len()..];
            if let Some(name) = rest.split('/').next().filter(|n| !n.is_empty()) {
                names.insert(name);
            }
        }
        for name in names {
            if out.len() >= LIST_FILES_MAX {
                return;
            }
            let path = child(dir, name);
            if name.starts_with('.') || name == "target" || skip.contains(&path) {
                continue;
            }
            if self.dirs.contains(&path) {
                if depth < LIST_DEPTH_MAX {
                    self.list(&path, depth + 1, skip, out);
                }
            } else {
                out.push(PathBuf::from(path));
            }
        }
    }

    /// Where an imported project goes and its files (web spec §3): `__MACOSX/` dropped, a
    /// directory shared by every entry stripped (and used as the name, else `hint`), the
    /// name reduced to `A–Z a–z 0–9 . _ -`, and `-2`, `-3`, … appended until no such
    /// directory exists under [`PROJECTS`]. Returns (project directory, entries relative to
    /// it).
    ///
    /// # Errors
    ///
    /// No files, more than [`LIST_FILES_MAX`] files or [`IMPORT_BYTES_MAX`] bytes, an entry
    /// path that is absolute or has an empty, `.` or `..` segment, a `\`, `:` or NUL (the
    /// archive is refused whole: it is a trust boundary), or 99 projects of that name.
    pub(crate) fn import_plan(
        &self,
        hint: &str,
        mut entries: Vec<(String, Vec<u8>)>,
    ) -> Result<(String, Vec<(String, Vec<u8>)>), String> {
        entries.retain(|(p, _)| !p.starts_with("__MACOSX/"));
        if entries.is_empty() {
            return Err("no files to import".into());
        }
        if entries.len() > LIST_FILES_MAX {
            return Err(format!("more than {LIST_FILES_MAX} files"));
        }
        let total: usize = entries.iter().map(|e| e.1.len()).sum();
        if total > IMPORT_BYTES_MAX {
            return Err(format!("more than {} MiB", IMPORT_BYTES_MAX >> 20));
        }
        for (p, _) in &entries {
            let bad = p
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == ".." || s.contains(['\\', ':', '\0']));
            if bad {
                return Err(format!("{p:?}: unsafe path in the archive"));
            }
        }
        let first = |p: &str| p.split_once('/').map(|(top, _)| top.to_owned());
        let top = first(&entries[0].0)
            .filter(|t| entries.iter().all(|(p, _)| first(p).as_deref() == Some(t)));
        if let Some(t) = &top {
            for e in &mut entries {
                e.0.drain(..=t.len());
            }
        }
        let raw = top.as_deref().unwrap_or(hint);
        let clean: String = raw
            .chars()
            .map(|c| match c {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => c,
                _ => '_',
            })
            .take(64)
            .collect();
        let name = match clean.trim_start_matches('.') {
            "" => "project",
            n => n,
        };
        let dir = (1..100)
            .map(|k| match k {
                1 => format!("{PROJECTS}/{name}"),
                k => format!("{PROJECTS}/{name}-{k}"),
            })
            .find(|d| !self.exists(d))
            .ok_or_else(|| format!("99 projects named {name} already"))?;
        assert!(
            entries
                .iter()
                .all(|(p, _)| !p.is_empty() && !p.starts_with('/'))
        );
        Ok((dir, entries))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift64*: seed-replayable inputs without a dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
            xs[(self.next() % xs.len() as u64) as usize]
        }
    }

    /// Paths that collide in every way the spines can make them: a file where a directory
    /// is wanted and back, hidden and `target` subtrees, names that sort around `/`.
    const PATHS: &[&str] = &[
        "/a",
        "/a/b",
        "/a/b/c.shcomp",
        "/a/b-c",
        "/a/b.d",
        "/top.shcomp",
        "/d/.schemify/journal",
        "/d/target/x",
        "/d/e/f/g/h.txt",
        "/d/e.txt",
        "/z",
    ];

    /// Model test: random operation scripts give the same answers on the table as on the
    /// native file system (`std::fs` through the native `platform` code), the oracle.
    #[test]
    fn behaves_like_the_native_file_system() {
        for seed in 1..=40u64 {
            let root =
                std::env::temp_dir().join(format!("schemify-vfs-{}-{seed}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("temp dir");
            let native = |k: &str| root.join(&k[1..]);
            let mut v = Vfs::new();
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            for step in 0..120 {
                let k = rng.pick(PATHS);
                let bytes = format!("{seed}:{step}").into_bytes();
                let ctx = format!("seed {seed} step {step} {k}");
                match rng.next() % 6 {
                    0 | 1 => assert_eq!(
                        v.write(k, bytes.clone(), 1).is_ok(),
                        super::super::imp::write_atomic(&native(k), &bytes).is_ok(),
                        "write {ctx}"
                    ),
                    2 => assert_eq!(
                        v.append(k, &bytes, 1).is_ok(),
                        super::super::imp::append(&native(k), &bytes).is_ok(),
                        "append {ctx}"
                    ),
                    3 => assert_eq!(
                        v.remove(k).is_ok(),
                        super::super::imp::remove(&native(k)).is_ok(),
                        "remove {ctx}"
                    ),
                    _ => {}
                }
                for &p in PATHS {
                    let n = native(p);
                    assert_eq!(
                        v.read(p),
                        std::fs::read(&n).ok().as_deref(),
                        "read {p} ({ctx})"
                    );
                    assert_eq!(v.is_dir(p), n.is_dir(), "is_dir {p} ({ctx})");
                    assert_eq!(v.exists(p), n.exists(), "exists {p} ({ctx})");
                }
                for (dir, skip) in [
                    ("/", vec![]),
                    ("/d", vec!["/d/e".to_owned()]),
                    ("/a", vec![]),
                ] {
                    let mut got = Vec::new();
                    v.list(dir, 0, &skip, &mut got);
                    let mut want = Vec::new();
                    let skip_n: Vec<PathBuf> = skip.iter().map(|s| native(s)).collect();
                    super::super::imp::list(&native(dir), 0, &skip_n, &mut want);
                    // A rename onto a directory leaves the writer's temp file behind natively.
                    let want: Vec<String> = want
                        .iter()
                        .filter(|p| !p.to_string_lossy().ends_with(".schemify-tmp"))
                        .map(|p| {
                            key(Path::new("/")
                                .join(p.strip_prefix(&root).expect("under root"))
                                .as_path())
                        })
                        .collect();
                    let got: Vec<String> = got.iter().map(|p| key(p)).collect();
                    assert_eq!(got, want, "list {dir} ({ctx})");
                }
            }
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn keys_are_absolute_and_normalized() {
        assert_eq!(key(Path::new("/")), "/");
        assert_eq!(key(Path::new("/projects/x/")), "/projects/x");
        assert_eq!(key(Path::new("/a/./b/../c")), "/a/c");
        assert_eq!(
            key(Path::new("rel/f")),
            "/rel/f",
            "relative to the page's /"
        );
        assert_eq!(key(Path::new("/..")), "/");
        assert_eq!(parent("/a/b"), Some("/a"));
        assert_eq!(parent("/a"), Some("/"));
        assert_eq!(parent("/"), None);
    }

    #[test]
    fn the_flush_queue_keeps_the_order_of_last_changes() {
        let mut v = Vfs::new();
        v.write("/p/top.shcomp", b"v1".to_vec(), 1).expect("writes");
        v.append("/p/.schemify/journal", b"op\n", 1)
            .expect("appends");
        v.write("/p/top.shcomp", b"v2".to_vec(), 2).expect("writes");
        v.remove("/p/.schemify/journal").expect("removes");
        // Save order: the cell reaches OPFS before its journal is dropped.
        assert_eq!(v.next_stale().as_deref(), Some("/p/top.shcomp"));
        assert_eq!(
            v.read("/p/top.shcomp"),
            Some(b"v2".as_slice()),
            "latest bytes flush"
        );
        assert_eq!(v.next_stale().as_deref(), Some("/p/.schemify/journal"));
        assert_eq!(v.read("/p/.schemify/journal"), None, "flushes as a removal");
        assert!(!v.has_stale() && v.next_stale().is_none());
        // Mounted files are not stale.
        v.load("/m/x", b"x".to_vec(), 5).expect("loads");
        assert!(!v.has_stale());
        assert_eq!(v.mtime("/m/x"), Some(5));
        assert!(v.is_dir("/m"));
    }

    #[test]
    fn the_byte_budget_holds() {
        let mut v = Vfs::new();
        v.write("/a", vec![0; 10], 0).expect("writes");
        v.write("/a", vec![0; 4], 0).expect("shrinks");
        v.remove("/a").expect("removes");
        assert_eq!(v.bytes, 0);
        assert!(
            v.write("/", Vec::new(), 0).is_err(),
            "the root is a directory"
        );
    }

    #[test]
    fn imports_strip_the_shared_top_directory_and_refuse_unsafe_paths() {
        let mut v = Vfs::new();
        let e = |p: &str| (p.to_owned(), p.as_bytes().to_vec());
        let (dir, files) = v
            .import_plan(
                "inv-main",
                vec![
                    e("inv/top.shcomp"),
                    e("inv/lib/a.shprim"),
                    e("__MACOSX/inv/._top.shcomp"),
                ],
            )
            .expect("imports");
        assert_eq!(dir, "/projects/inv");
        let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(names, ["top.shcomp", "lib/a.shprim"]);
        assert_eq!(files[0].1, b"inv/top.shcomp", "bytes kept");
        // No shared directory: the hint names it; taken names get a suffix.
        v.mkdirs("/projects/my_design").expect("mkdirs");
        let (dir, files) = v
            .import_plan("my design", vec![e("a.shcomp"), e("b/c.shcomp")])
            .expect("imports");
        assert_eq!(dir, "/projects/my_design-2");
        assert_eq!(files.len(), 2);
        let (dir, _) = v.import_plan("..", vec![e("x")]).expect("imports");
        assert_eq!(dir, "/projects/project");
        for bad in [
            "../etc/passwd",
            "/abs",
            "a//b",
            "a/./b",
            "a\\b",
            "c:x",
            "a/\0",
        ] {
            assert!(v.import_plan("p", vec![e(bad)]).is_err(), "{bad:?} refused");
        }
        assert!(v.import_plan("p", Vec::new()).is_err());
    }
}
