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
//! clipboard, one window.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::js_sys::{self, Uint8Array};
use web_sys::wasm_bindgen::closure::Closure;
use web_sys::wasm_bindgen::{JsCast, JsValue};
use web_sys::{FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemHandle};

use super::vfs::{self, IMPORT_BYTES_MAX, Vfs};
use super::{CLIPBOARD_BYTES_MAX, CLIPBOARD_PENDING, WEB_ROOT};

/// Directory depth the mount walks (a job directory sits 5 levels under a project).
const MOUNT_DEPTH_MAX: u32 = 32;
/// Tick period while the browser owes an answer (a clipboard read).
const POLL_MS: u32 = 20;
/// How long an import waits for its files to reach OPFS before reloading.
const IMPORT_WAIT_MS: i32 = 30_000;
const NO_WINDOW: &str = "no browser window";

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
    static FS: RefCell<Vfs> = RefCell::new(Vfs::new());
    /// The OPFS root directory; `None` until mounted, or when the browser has no OPFS (the
    /// page then edits in memory only).
    static ROOT: RefCell<Option<FileSystemDirectoryHandle>> = const { RefCell::new(None) };
    /// The flusher task runs.
    static FLUSHING: Cell<bool> = const { Cell::new(false) };
    /// OPFS writes that failed this session (the unload guard warns while non-zero).
    static FAILED: Cell<u32> = const { Cell::new(0) };
    static CLIP: RefCell<Clip> = RefCell::new(Clip::default());
    /// The object URL of the last download, released by the next one.
    static LAST_URL: RefCell<Option<String>> = const { RefCell::new(None) };
}

// ------------------------------------------------------------------ the platform API

pub fn read(path: &Path) -> Result<Vec<u8>, String> {
    let k = vfs::key(path);
    FS.with_borrow(|fs| fs.read(&k).map(<[u8]>::to_vec))
        .ok_or_else(|| format!("{}: no such file", path.display()))
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

pub fn mtime_unix_s(path: &Path) -> Option<u64> {
    FS.with_borrow(|fs| fs.mtime(&vfs::key(path)))
}

pub fn list(dir: &Path, depth: u32, skip: &[PathBuf], out: &mut Vec<PathBuf>) {
    let skip: Vec<String> = skip.iter().map(|p| vfs::key(p)).collect();
    FS.with_borrow(|fs| fs.list(&vfs::key(dir), depth, &skip, out));
}

pub fn state_dir() -> Option<PathBuf> {
    schemify_config::dirs::here(schemify_config::dirs::Dir::State).ok()
}

pub fn spawn(_: &Path, _: &[String], _: &[u8]) -> (i32, Vec<u8>) {
    // D16: no processes in a browser; simulation goes through runner plugins.
    (-1, b"no processes on the web".to_vec())
}

pub fn unix_ms() -> u64 {
    js_sys::Date::now() as u64
}

pub fn seed() -> u64 {
    // ponytail: `Math.random` (not a CSPRNG) seeds object ids; ids need uniqueness, not
    // secrecy. `crypto.getRandomValues` if they ever must be unguessable.
    let half = |r: f64| (r * f64::from(u32::MAX)) as u64;
    (half(js_sys::Math::random()) << 32) ^ half(js_sys::Math::random()) ^ unix_ms()
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

// ------------------------------------------------------------------ clipboard (§4)

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
                        cut -= 1;
                    }
                    text.truncate(cut);
                }
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
    CLIP.with_borrow_mut(|c| std::mem::take(&mut c.ready))
}

pub fn wake_ms() -> Option<u32> {
    CLIP.with_borrow(|c| (c.reading || c.ready).then_some(POLL_MS))
}

/// `navigator.clipboard`, absent outside secure contexts (plain `http://` other than
/// localhost).
fn clipboard() -> Result<web_sys::Clipboard, String> {
    let c = web_sys::window().ok_or(NO_WINDOW)?.navigator().clipboard();
    if AsRef::<JsValue>::as_ref(&c).is_undefined() {
        return Err("no clipboard here (the page needs HTTPS)".into());
    }
    Ok(c)
}

// ------------------------------------------------------------------ projects in and out (§3)

pub fn export(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map_or_else(|| "export".to_owned(), |n| n.to_string_lossy().into_owned());
    let window = web_sys::window().ok_or(NO_WINDOW)?;
    let doc = window.document().ok_or(NO_WINDOW)?;
    let parts = js_sys::Array::of1(&JsValue::from(Uint8Array::from(bytes)));
    let blob = web_sys::Blob::new_with_u8_array_sequence(&JsValue::from(parts)).map_err(js_err)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js_err)?;
    let a: web_sys::HtmlAnchorElement = doc
        .create_element("a")
        .map_err(js_err)?
        .dyn_into()
        .map_err(|_| "no anchor element".to_owned())?;
    a.set_href(&url);
    a.set_download(&name);
    a.click();
    // The previous download started long ago; its blob can go now.
    if let Some(old) = LAST_URL.replace(Some(url)) {
        let _ = web_sys::Url::revoke_object_url(&old);
    }
    Ok(())
}

pub fn import_project(folder: bool) -> Result<(), String> {
    let doc = web_sys::window()
        .and_then(|w| w.document())
        .ok_or(NO_WINDOW)?;
    let input: web_sys::HtmlInputElement = doc
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

/// Reads the picked files (one zip, or a folder's files), writes them under
/// `/projects/<name>`, waits until OPFS has them, and reloads the page on that project (one
/// project per session, like one per native process).
async fn import(files: web_sys::FileList, folder: bool) -> Result<(), String> {
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
            let rel = js_sys::Reflect::get(&f, &JsValue::from_str("webkitRelativePath"))
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
    let dir = FS.with_borrow_mut(|fs| {
        let (dir, files) = fs.import_plan(&hint, entries)?;
        for (rel, bytes) in files {
            fs.write(&format!("{dir}/{rel}"), bytes, now_s())?;
        }
        Ok::<_, String>(dir)
    })?;
    let failed = FAILED.get();
    flush();
    let mut waited = 0;
    while pending() && waited < IMPORT_WAIT_MS {
        sleep(50).await;
        waited += 50;
    }
    if pending() || FAILED.get() != failed {
        return Err(format!("{dir}: not all files reached web storage"));
    }
    let loc = web_sys::window().ok_or(NO_WINDOW)?.location();
    loc.set_hash(&dir).map_err(js_err)?;
    loc.reload().map_err(js_err)
}

// ------------------------------------------------------------------ OPFS (§2)

pub fn mount_then(run: impl FnOnce() -> Result<(), String> + 'static) {
    spawn_local(async move {
        match mount().await {
            Ok(n) => log(&format!("web storage: {n} files")),
            Err(e) => error(&format!(
                "web storage: {e}; edits stay in this page (nothing persists)"
            )),
        }
        if let Err(e) = FS.with_borrow_mut(|fs| fs.mkdirs(WEB_ROOT)) {
            error(&format!("web storage: {e}"));
        }
        if let Some(w) = web_sys::window() {
            guard_unload(&w);
        }
        if let Err(e) = run() {
            error(&format!("schemify: {e}"));
        }
    });
}

/// Loads every OPFS file into the table; returns how many.
async fn mount() -> Result<usize, String> {
    let storage = web_sys::window().ok_or(NO_WINDOW)?.navigator().storage();
    let root: FileSystemDirectoryHandle = JsFuture::from(storage.get_directory())
        .await
        .map_err(js_err)?
        .unchecked_into();
    ROOT.with_borrow_mut(|r| *r = Some(root.clone()));
    let mut todo = vec![(root, "/".to_owned(), 0u32)];
    let mut files = 0usize;
    // Bounded: every directory is visited once and the table refuses past FILES_MAX.
    while let Some((dir, key, depth)) = todo.pop() {
        let entries = dir.values();
        loop {
            let next = entries.next().map_err(js_err)?;
            let next: js_sys::IteratorNext =
                JsFuture::from(next).await.map_err(js_err)?.unchecked_into();
            if next.done() {
                break;
            }
            let h: FileSystemHandle = next.value().unchecked_into();
            let name = h.name();
            let child = if key == "/" {
                format!("/{name}")
            } else {
                format!("{key}/{name}")
            };
            if h.kind() == web_sys::FileSystemHandleKind::Directory {
                FS.with_borrow_mut(|fs| fs.mkdirs(&child))?;
                if depth < MOUNT_DEPTH_MAX {
                    todo.push((h.unchecked_into(), child, depth + 1));
                }
            } else if !name.ends_with(".crswap") {
                // `.crswap`: Chrome's temporary for an unfinished `createWritable`.
                let fh: FileSystemFileHandle = h.unchecked_into();
                let file: web_sys::File = JsFuture::from(fh.get_file())
                    .await
                    .map_err(js_err)?
                    .unchecked_into();
                let buf = JsFuture::from(file.array_buffer()).await.map_err(js_err)?;
                let bytes = Uint8Array::new(&buf).to_vec();
                let mtime = (file.last_modified() / 1000.0) as u64;
                FS.with_borrow_mut(|fs| fs.load(&child, bytes, mtime))?;
                files += 1;
            }
        }
    }
    Ok(files)
}

/// Starts the flusher unless it runs: it writes each stale path's current bytes to OPFS (or
/// removes it), oldest change first, until nothing is stale.
fn flush() {
    if FLUSHING.get() {
        return;
    }
    FLUSHING.set(true);
    spawn_local(async {
        let mounted = ROOT.with_borrow(Option::is_some);
        while let Some(key) = FS.with_borrow_mut(Vfs::next_stale) {
            if !mounted {
                continue; // memory only: nothing to write to
            }
            let bytes = FS.with_borrow(|fs| fs.read(&key).map(<[u8]>::to_vec));
            let done = match &bytes {
                Some(b) => opfs_write(&key, b).await,
                None => opfs_remove(&key).await,
            };
            match done {
                Ok(()) => debug(&format!(
                    "web storage: {} {key}",
                    bytes.map_or_else(|| "removed".to_owned(), |b| format!("wrote {} B", b.len()))
                )),
                Err(e) => {
                    FAILED.set(FAILED.get() + 1);
                    error(&format!("web storage: {key}: {e}"));
                }
            }
        }
        FLUSHING.set(false);
    });
}

/// Changes on their way to OPFS.
fn pending() -> bool {
    FLUSHING.get() || FS.with_borrow(Vfs::has_stale)
}

/// Changes that have not reached OPFS: pending, or failed this session.
fn unsaved() -> bool {
    pending() || FAILED.get() > 0
}

/// Asks before the page closes while edits are still on their way to OPFS.
fn guard_unload(w: &web_sys::Window) {
    let guard =
        Closure::<dyn FnMut(web_sys::BeforeUnloadEvent)>::new(|e: web_sys::BeforeUnloadEvent| {
            if unsaved() {
                e.prevent_default();
                e.set_return_value("unsaved");
            }
        });
    if w.add_event_listener_with_callback("beforeunload", guard.as_ref().unchecked_ref())
        .is_ok()
    {
        guard.forget(); // lives as long as the page
    }
}

/// The OPFS directory holding `key` and the file name, creating directories when `create`.
async fn opfs_parent(
    key: &str,
    create: bool,
) -> Result<(FileSystemDirectoryHandle, String), String> {
    let mut dir = ROOT
        .with_borrow(Clone::clone)
        .ok_or("web storage is not mounted")?;
    let mut segs: Vec<&str> = key.split('/').filter(|s| !s.is_empty()).collect();
    let name = segs.pop().ok_or("the root is not a file")?.to_owned();
    let opts = web_sys::FileSystemGetDirectoryOptions::new();
    opts.set_create(create);
    for s in segs {
        dir = JsFuture::from(dir.get_directory_handle_with_options(s, &opts))
            .await
            .map_err(js_err)?
            .unchecked_into();
    }
    Ok((dir, name))
}

async fn opfs_write(key: &str, bytes: &[u8]) -> Result<(), String> {
    let (dir, name) = opfs_parent(key, true).await?;
    let opts = web_sys::FileSystemGetFileOptions::new();
    opts.set_create(true);
    let file: FileSystemFileHandle = JsFuture::from(dir.get_file_handle_with_options(&name, &opts))
        .await
        .map_err(js_err)?
        .unchecked_into();
    let w: web_sys::FileSystemWritableFileStream = JsFuture::from(file.create_writable())
        .await
        .map_err(js_err)?
        .unchecked_into();
    // A copy owned by JS: a view into wasm memory could move while the write is queued.
    let data = Uint8Array::from(bytes);
    JsFuture::from(w.write_with_js_u8_array(&data).map_err(js_err)?)
        .await
        .map_err(js_err)?;
    JsFuture::from(w.close()).await.map_err(js_err)?;
    Ok(())
}

async fn opfs_remove(key: &str) -> Result<(), String> {
    let (dir, name) = match opfs_parent(key, false).await {
        Ok(p) => p,
        Err(_) => return Ok(()), // its directory is gone: so is the file
    };
    let opts = web_sys::FileSystemRemoveOptions::new();
    opts.set_recursive(true);
    match JsFuture::from(dir.remove_entry_with_options(&name, &opts)).await {
        Ok(_) => Ok(()),
        Err(e) if dom_error(&e) == Some("NotFoundError".into()) => Ok(()),
        Err(e) => Err(js_text(&e)),
    }
}

// ------------------------------------------------------------------ helpers

fn now_s() -> u64 {
    unix_ms() / 1000
}

/// Resolves after `ms` milliseconds (a `setTimeout` promise).
async fn sleep(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(p).await;
}

fn dom_error(e: &JsValue) -> Option<String> {
    e.dyn_ref::<web_sys::DomException>()
        .map(web_sys::DomException::name)
}

fn js_text(e: &JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    if let Some(err) = e.dyn_ref::<js_sys::Error>() {
        return String::from(err.message());
    }
    format!("{e:?}")
}

fn js_err(e: JsValue) -> String {
    js_text(&e)
}

fn log(msg: &str) {
    web_sys::console::log_1(&JsValue::from_str(msg));
}

/// A console line hidden by default (DevTools' "Verbose" level): one per OPFS write.
fn debug(msg: &str) {
    web_sys::console::debug_1(&JsValue::from_str(msg));
}

fn warn(msg: &str) {
    web_sys::console::warn_1(&JsValue::from_str(msg));
}

fn error(msg: &str) {
    web_sys::console::error_1(&JsValue::from_str(msg));
}
