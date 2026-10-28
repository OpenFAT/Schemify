//! OPFS write-back for the web build (web spec §2, §3.1; D31): the browser's OPFS tree →
//! the page's `vfs` table at mount, and the table's stale paths → OPFS, in the order of
//! their last change, by one flusher task. Also copies a hosted project the URL names into
//! the table before the editor starts, and guards the page's unload while writes are on
//! their way.
//!
//! Contents (for `web`): `fn mount_then`, `fn flush`, `fn pending`.

use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::js_sys::{self, Uint8Array};
use web_sys::wasm_bindgen::closure::Closure;
use web_sys::wasm_bindgen::{JsCast, JsValue};
use web_sys::{FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemHandle};

use super::WEB_ROOT;
use super::vfs::{self, IMPORT_BYTES_MAX, Vfs};
use super::web::{
    FAILED, FLUSHING, FS, NO_WINDOW, ROOT, args, debug, error, js_err, js_text, log, now_s,
};

/// Directory depth the mount walks (a job directory sits 5 levels under a project).
const MOUNT_DEPTH_MAX: u32 = 32;

/// Mounts the web storage, copies a hosted project the URL names, guards the unload, then
/// calls `run` (`platform::mount_then`).
pub(super) fn mount_then(run: impl FnOnce() -> Result<(), String> + 'static) {
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
        if let Err(e) = hosted().await {
            error(&format!("hosted project: {e}"));
        }
        if let Some(w) = web_sys::window() {
            guard_unload(&w);
        }
        if let Err(e) = run() {
            error(&format!("schemify: {e}"));
        }
    });
}

/// Starts the flusher unless it runs: it writes each stale path's current bytes to OPFS (or
/// removes it), oldest change first, until nothing is stale.
pub(super) fn flush() {
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
pub(super) fn pending() -> bool {
    FLUSHING.get() || FS.with_borrow(Vfs::has_stale)
}

// ---- private ----

/// Copies the hosted project the URL names into storage (web spec §3.1), when the page's
/// own site serves one and its manifest differs from the copy stored last time. A site
/// without the manifest (any non-200 answer) is not an error: the project is a local one.
///
/// ponytail: fetches one file at a time (a few hundred small files take a second or two);
/// a bounded set of concurrent fetches if hosted projects grow large.
async fn hosted() -> Result<(), String> {
    use crate::hosted;
    let Some(arg) = args().into_iter().next() else {
        return Ok(());
    };
    let Some(name) = hosted::project_name(&arg) else {
        return Ok(());
    };
    let base = format!("projects/{name}");
    let Some(text) = fetch(&format!("{base}/{}", hosted::MANIFEST)).await? else {
        return Ok(());
    };
    let text = String::from_utf8(text).map_err(|e| format!("{}: {e}", hosted::MANIFEST))?;
    let dir = format!("{}/{name}", vfs::PROJECTS);
    let stamp = format!("{dir}/{}", hosted::STAMP);
    if FS.with_borrow(|fs| fs.read(&stamp) == Some(text.as_bytes())) {
        return Ok(()); // the same build: keep local edits
    }
    let rows = hosted::parse(&text)?;
    let mut files = Vec::with_capacity(rows.len() + 1);
    let mut total = 0usize;
    for (rel, digest) in rows {
        let bytes = fetch(&format!("{base}/{rel}"))
            .await?
            .ok_or_else(|| format!("{base}/{rel}: not on the site"))?;
        if !hosted::matches(&digest, &bytes) {
            return Err(format!("{base}/{rel}: does not match {}", hosted::MANIFEST));
        }
        total += bytes.len();
        if total > IMPORT_BYTES_MAX {
            return Err(format!("more than {} MiB", IMPORT_BYTES_MAX >> 20));
        }
        files.push((rel, bytes));
    }
    // The stamp last: a refresh cut short is retried on the next load.
    files.push((hosted::STAMP.to_owned(), text.into_bytes()));
    let n = files.len() - 1;
    FS.with_borrow_mut(|fs| fs.replace_tree(&dir, files, now_s()))?;
    flush();
    log(&format!("hosted project {name}: {n} files from the site"));
    Ok(())
}

/// The body of `url` (relative to the page) when the server answers 200; `None` for any
/// other status.
///
/// # Errors
///
/// No window, or the request failed without an answer (network, CORS).
async fn fetch(url: &str) -> Result<Option<Vec<u8>>, String> {
    let w = web_sys::window().ok_or(NO_WINDOW)?;
    let resp: web_sys::Response = JsFuture::from(w.fetch_with_str(url))
        .await
        .map_err(|e| format!("{url}: {}", js_err(e)))?
        .unchecked_into();
    if resp.status() != 200 {
        return Ok(None);
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(js_err)?)
        .await
        .map_err(js_err)?;
    Ok(Some(Uint8Array::new(&buf).to_vec()))
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

/// The `DOMException` name of a JS error, if it is one.
fn dom_error(e: &JsValue) -> Option<String> {
    e.dyn_ref::<web_sys::DomException>()
        .map(web_sys::DomException::name)
}
