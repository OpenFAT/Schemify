//! The browser side of the OS boundary (`wasm32-unknown-unknown`; D2, D31, D32; web spec).
//! Everything here is web-sys / js-sys generated glue called from Rust: no hand-written JS.
//!
//! Storage: OPFS is asynchronous on the main thread and the spines are synchronous, so the
//! page keeps the OPFS tree in a [`Vfs`] (mounted before the editor starts) and one flusher
//! task writes changes back in the order they were made (web spec §2). The clipboard is the
//! async Clipboard API: a paste that has to wait for the browser runs again when the text
//! arrives (§4). Exports are downloads (§3).
//!
//! The tables below are page-wide like the OS state they stand in for: one file system, one
//! clipboard, one window. Mounting and writing back to OPFS is `opfs`'s.
//!
//! Contents (for `platform`, each documented there): `read`, `read_tail`, `exists`, `is_dir`,
//! `write_atomic`, `append`, `remove`, `remove_tree`, `mtime_unix_s`, `list`, `spawn`,
//! `unix_ms`, `seed`, `args`, `clipboard_set`, `clipboard_get`, `clipboard_ready`,
//! `wake_ms`, `export`, `import_project`, `mount_then`; for `opfs`: the tables `FS`, `ROOT`,
//! `FLUSHING`, `FAILED`, `NO_WINDOW`, and `now_s`, `js_text`, `js_err`, `log`, `debug`,
//! `error`.

use std::cell::{Cell, RefCell};
use std::mem;
use std::path::{Path, PathBuf};

use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::js_sys::{self, Array, Date, Math, Promise, Reflect, Uint8Array};
use web_sys::wasm_bindgen::closure::Closure;
use web_sys::wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    Blob, Clipboard, FileList, FileSystemDirectoryHandle, HtmlAnchorElement, HtmlInputElement, Url,
    console,
};

pub(super) use super::opfs::mount_then;
use super::opfs::{flush, pending};
use super::vfs::{self, IMPORT_BYTES_MAX, ImportPlan, Vfs};
use super::{CLIPBOARD_BYTES_MAX, CLIPBOARD_PENDING};

/// The error when the page has no window (a worker).
pub(super) const NO_WINDOW: &str = "no browser window";
/// Tick period while the browser owes an answer (a clipboard read).
const POLL_MS: u32 = 20;
/// How long an import waits for its files to reach OPFS before reloading.
const IMPORT_WAIT_MS: i32 = 30_000;

/// The clipboard round trip (web spec §4).
#[derive(Default)]
struct Clip {
    /// A `readText` is in flight.
    reading: bool,
    /// Its answer, for the next [`clipboard_get`].
    answer: Option<Result<String, String>>,
    /// The answer arrived and the editor has not been told yet.
    ready: bool,
}

thread_local! {
    /// The page's file system.
    pub(super) static FS: RefCell<Vfs> = RefCell::new(Vfs::new());
    /// The OPFS root directory; `None` until mounted, or when the browser has no OPFS (the
    /// page then edits in memory only).
    pub(super) static ROOT: RefCell<Option<FileSystemDirectoryHandle>> = const { RefCell::new(None) };
    /// The flusher task runs.
    pub(super) static FLUSHING: Cell<bool> = const { Cell::new(false) };
    /// OPFS writes that failed this session (the unload guard warns while non-zero).
    pub(super) static FAILED: Cell<u32> = const { Cell::new(0) };
    static CLIP: RefCell<Clip> = RefCell::new(Clip::default());
    /// The object URL of the last download, released by the next one.
    static LAST_URL: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub fn read(path: &Path) -> Result<Vec<u8>, String> {
    let k = vfs::key(path);
    FS.with_borrow(|fs| fs.read(&k).map(<[u8]>::to_vec))
        .ok_or_else(|| format!("{}: no such file", path.display()))
}

pub fn read_tail(path: &Path, max: u64) -> Result<(Vec<u8>, bool), String> {
    let k = vfs::key(path);
    let max_bytes = usize::try_from(max).unwrap_or(usize::MAX);
    let (tail, cut) = FS
        .with_borrow(|fs| {
            fs.read(&k).map(|b| {
                let from = b.len().saturating_sub(max_bytes);
                (b[from..].to_vec(), from > 0)
            })
        })
        .ok_or_else(|| format!("{}: no such file", path.display()))?;
    if cut {
        assert_eq!(tail.len(), max_bytes, "a cut tail is exactly `max` bytes");
    }
    Ok((tail, cut))
}

pub fn exists(path: &Path) -> bool {
    FS.with_borrow(|fs| fs.exists(&vfs::key(path)))
}

pub fn is_dir(path: &Path) -> bool {
    FS.with_borrow(|fs| fs.is_dir(&vfs::key(path)))
}

/// OPFS's `createWritable` + `close` swaps the file in whole, so the flushed write is atomic
/// as well.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let k = vfs::key(path);
    FS.with_borrow_mut(|fs| fs.write(&k, bytes.to_vec(), now_s()))
        .inspect_err(|e| warn(e))?;
    flush();
    assert!(exists(path));
    Ok(())
}

pub fn append(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let k = vfs::key(path);
    FS.with_borrow_mut(|fs| fs.append(&k, bytes, now_s()))
        .inspect_err(|e| warn(e))?;
    flush();
    Ok(())
}

pub fn remove(path: &Path) -> Result<(), String> {
    let k = vfs::key(path);
    FS.with_borrow_mut(|fs| fs.remove(&k))?;
    flush();
    Ok(())
}

pub fn remove_tree(path: &Path) -> Result<(), String> {
    if !is_dir(path) {
        return remove(path);
    }
    // ponytail: hidden entries (`.x`) are skipped by `list`, so they stay; the PDK store
    // writes none. Add a raw prefix walk to the vfs if one ever does.
    let mut files = Vec::new();
    list(path, 0, &[], &mut files);
    for f in &files {
        remove(f)?;
    }
    Ok(())
}

pub fn mtime_unix_s(path: &Path) -> Option<u64> {
    FS.with_borrow(|fs| fs.mtime(&vfs::key(path)))
}

pub fn list(dir: &Path, depth: u32, skip: &[&Path], out: &mut Vec<PathBuf>) {
    let skip: Vec<String> = skip.iter().map(|p| vfs::key(p)).collect();
    FS.with_borrow(|fs| fs.list(&vfs::key(dir), depth, &skip, out));
}

pub fn spawn(_: &Path, _: &[String], _: &[u8]) -> (i32, Vec<u8>) {
    // D16: no processes in a browser; simulation goes through runner plugins.
    (-1, b"no processes on the web".to_vec())
}

pub fn unix_ms() -> u64 {
    Date::now() as u64
}

pub fn seed() -> u64 {
    // ponytail: `Math.random` (not a CSPRNG) seeds object ids; ids need uniqueness, not
    // secrecy. `crypto.getRandomValues` if they ever must be unguessable.
    let half = |r: f64| (r * f64::from(u32::MAX)) as u64;
    (half(Math::random()) << 32) ^ half(Math::random()) ^ unix_ms()
}

pub fn args() -> Vec<String> {
    let Some(w) = web_sys::window() else {
        return Vec::new();
    };
    let hash = w.location().hash().unwrap_or_default();
    let raw = hash.strip_prefix('#').unwrap_or(&hash);
    if raw.is_empty() {
        return Vec::new();
    }
    let path = js_sys::decode_uri_component(raw).map_or_else(|_| raw.to_owned(), String::from);
    vec![path]
}

pub fn clipboard_set(text: &str) -> Result<(), String> {
    let done = clipboard()?.write_text(text);
    spawn_local(async move {
        if let Err(e) = JsFuture::from(done).await {
            warn(&format!("clipboard write: {}", js_text(&e)));
        }
    });
    Ok(())
}

pub fn clipboard_get() -> Result<String, String> {
    if let Some(answer) = CLIP.with_borrow_mut(|c| c.answer.take()) {
        return answer;
    }
    if CLIP.with_borrow(|c| c.reading) {
        return Err(CLIPBOARD_PENDING.into());
    }
    let read = clipboard()?.read_text();
    CLIP.with_borrow_mut(|c| c.reading = true);
    spawn_local(async move {
        let answer = match JsFuture::from(read).await {
            Ok(v) => {
                let mut text = v.as_string().unwrap_or_default();
                if text.len() > CLIPBOARD_BYTES_MAX {
                    let mut cut = CLIPBOARD_BYTES_MAX;
                    while !text.is_char_boundary(cut) {
                        // A UTF-8 character is at most four bytes long.
                        assert!(CLIPBOARD_BYTES_MAX - cut < 4);
                        cut -= 1;
                    }
                    text.truncate(cut);
                }
                assert!(text.len() <= CLIPBOARD_BYTES_MAX);
                Ok(text)
            }
            Err(e) => {
                let m = format!("clipboard read: {}", js_text(&e));
                warn(&m); // the editor falls back to its own clipboard silently
                Err(m)
            }
        };
        CLIP.with_borrow_mut(|c| {
            c.reading = false;
            c.answer = Some(answer);
            c.ready = true;
        });
    });
    Err(CLIPBOARD_PENDING.into())
}

pub fn clipboard_ready() -> bool {
    CLIP.with_borrow_mut(|c| mem::take(&mut c.ready))
}

pub fn wake_ms() -> Option<u32> {
    CLIP.with_borrow(|c| (c.reading || c.ready).then_some(POLL_MS))
}

pub fn export(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map_or_else(|| "export".to_owned(), |n| n.to_string_lossy().into_owned());
    let window = web_sys::window().ok_or(NO_WINDOW)?;
    let doc = window.document().ok_or(NO_WINDOW)?;
    let parts = Array::of1(&JsValue::from(Uint8Array::from(bytes)));
    let blob = Blob::new_with_u8_array_sequence(&JsValue::from(parts)).map_err(js_err)?;
    let url = Url::create_object_url_with_blob(&blob).map_err(js_err)?;
    let a: HtmlAnchorElement = doc
        .create_element("a")
        .map_err(js_err)?
        .dyn_into()
        .map_err(|_| "no anchor element".to_owned())?;
    a.set_href(&url);
    a.set_download(&name);
    a.click();
    // The previous download started long ago; its blob can go now.
    if let Some(old) = LAST_URL.replace(Some(url)) {
        let _ = Url::revoke_object_url(&old);
    }
    Ok(())
}

pub fn import_project(folder: bool) -> Result<(), String> {
    let doc = web_sys::window()
        .and_then(|w| w.document())
        .ok_or(NO_WINDOW)?;
    let input: HtmlInputElement = doc
        .create_element("input")
        .map_err(js_err)?
        .dyn_into()
        .map_err(|_| "no file input".to_owned())?;
    input.set_type("file");
    if folder {
        input.set_webkitdirectory(true);
    } else {
        input.set_accept(".zip,application/zip");
    }
    let picked = input.clone();
    let on_change = Closure::<dyn FnMut()>::once_into_js(move || {
        let Some(files) = picked.files() else {
            return;
        };
        spawn_local(async move {
            if let Err(e) = import(files, folder).await {
                error(&format!("import: {e}"));
            }
        });
    });
    input
        .add_event_listener_with_callback("change", on_change.unchecked_ref())
        .map_err(js_err)?;
    // Runs inside the menu click's user activation, which the picker requires.
    input.click();
    Ok(())
}

/// Seconds since the Unix epoch, the vfs's mtime unit.
pub(super) fn now_s() -> u64 {
    unix_ms() / 1000
}

/// A JS error value as text (its message for an `Error`).
pub(super) fn js_text(e: &JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    if let Some(err) = e.dyn_ref::<js_sys::Error>() {
        return String::from(err.message());
    }
    format!("{e:?}")
}

/// [`js_text`] for `map_err`.
pub(super) fn js_err(e: JsValue) -> String {
    js_text(&e)
}

/// A console line.
pub(super) fn log(msg: &str) {
    console::log_1(&JsValue::from_str(msg));
}

/// A console line hidden by default (DevTools' "Verbose" level): one per OPFS write.
pub(super) fn debug(msg: &str) {
    console::debug_1(&JsValue::from_str(msg));
}

/// A console error line.
pub(super) fn error(msg: &str) {
    console::error_1(&JsValue::from_str(msg));
}

// ---- private ----

/// `navigator.clipboard`, absent outside secure contexts (plain `http://` other than
/// localhost).
fn clipboard() -> Result<Clipboard, String> {
    let c = web_sys::window().ok_or(NO_WINDOW)?.navigator().clipboard();
    if AsRef::<JsValue>::as_ref(&c).is_undefined() {
        return Err("no clipboard here (the page needs HTTPS)".into());
    }
    Ok(c)
}

/// Reads the picked files (one zip, or a folder's files), writes them under
/// `/projects/<name>`, waits until OPFS has them, and reloads the page on that project (one
/// project per session, like one per native process).
async fn import(files: FileList, folder: bool) -> Result<(), String> {
    let mut entries = Vec::new();
    let mut hint = String::new();
    let mut total = 0usize;
    for i in 0..files.length() {
        let Some(f) = files.get(i) else {
            continue;
        };
        let buf = JsFuture::from(f.array_buffer()).await.map_err(js_err)?;
        let bytes = Uint8Array::new(&buf).to_vec();
        total += bytes.len();
        if total > IMPORT_BYTES_MAX {
            return Err(format!("more than {} MiB", IMPORT_BYTES_MAX >> 20));
        }
        if folder {
            // `File.webkitRelativePath` has no web-sys binding: read the property.
            let rel = Reflect::get(&f, &JsValue::from_str("webkitRelativePath"))
                .ok()
                .and_then(|v| v.as_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| f.name());
            entries.push((rel, bytes));
        } else {
            let name = f.name();
            hint = name.strip_suffix(".zip").unwrap_or(&name).to_owned();
            entries = crate::zip::read(&bytes, IMPORT_BYTES_MAX)?;
        }
    }
    assert!(total <= IMPORT_BYTES_MAX);
    let dir = FS.with_borrow_mut(|fs| {
        let ImportPlan { dir, files } = fs.import_plan(&hint, entries)?;
        for (rel, bytes) in files {
            fs.write(&format!("{dir}/{rel}"), bytes, now_s())?;
        }
        Ok::<String, String>(dir)
    })?;
    assert!(dir.starts_with(vfs::PROJECTS));
    let failed = FAILED.get();
    flush();
    let mut waited = 0;
    while pending() && waited < IMPORT_WAIT_MS {
        sleep(50).await;
        waited += 50;
    }
    assert!(waited <= IMPORT_WAIT_MS + 50);
    if pending() || FAILED.get() != failed {
        return Err(format!("{dir}: not all files reached web storage"));
    }
    let loc = web_sys::window().ok_or(NO_WINDOW)?.location();
    loc.set_hash(&dir).map_err(js_err)?;
    loc.reload().map_err(js_err)
}

/// Resolves after `ms` milliseconds (a `setTimeout` promise).
async fn sleep(ms: i32) {
    assert!(ms >= 0);
    let p = Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(p).await;
}

/// A console warning line.
fn warn(msg: &str) {
    console::warn_1(&JsValue::from_str(msg));
}
