//! Events → I/O chains → [`Model`] → panel. Every completion is matched to its request by
//! handle; at most [`PENDING_MAX`] requests are outstanding. Each step waits for the previous
//! one's `IoDone`, so nothing relies on the host executing requests in issue order.
//!
//! - refresh: read the index, the manifest and the lockfile (then each `STATE`); when the last
//!   read lands, recompute the references and run the action that asked for fresh data;
//! - `pdk.install F V`: remove `STATE` → add to the index → `ciel enable` → `schemify import`
//!   → write `tech.map` → write `STATE` = `ok`; a failing step stops the chain, so `STATE`
//!   stays absent (spec §3);
//! - `pdk.use F V`: refresh → refuse unless complete → read the import's manifest → write
//!   `schemify.project` (the app then runs `project.reload`);
//! - `pdk.remove F V` / `pdk.prune [confirm]`: refresh → refuse active/locked → `fs_remove`
//!   each version directory → rewrite the index → refresh.

use std::cell::RefCell;

use schemify_abi::guest::{self, Host};
use schemify_abi::{Code, Event, Kind, Level, Out, text};

use crate::{
    Fv, INDEX, Job, Model, RECIPES, parse_index, parse_versions, prune_plan, recipe, references,
    remove_refusal, tail, use_manifest, valid_version, version_dir, write_index,
};

guest::export!(on_event);

const PENDING_MAX: usize = 64;
const EMPTY_MANIFEST: &[u8] = b"schemify 1 project\n";

/// What a pending handle is for.
enum Io {
    Index,
    State(Fv),
    Manifest,
    Lock,
    ListRemote(String),
    ClearState,
    InstallIndex,
    Fetch,
    Import,
    Map,
    StateOk,
    UseImport(Fv),
    UseWrite(Fv),
    Remove(Fv),
    RemoveIndex,
}

/// What runs once a refresh lands.
enum Action {
    Use(Fv),
    Remove(Fv),
    Prune { confirm: bool },
}

#[derive(Default)]
struct State {
    m: Model,
    pending: Vec<(u32, Io)>,
    /// Refresh reads still outstanding.
    refreshing: u32,
    manifest: Option<Vec<u8>>,
    lock: Option<Vec<u8>>,
    after: Option<Action>,
    /// Removals of the running batch still outstanding, and the ones done.
    removing: u32,
    removed: Vec<Fv>,
}

thread_local! {
    /// Plugin state; the guest is single-threaded and the cell outlives events.
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn on_event(ev: Event<'_>, host: &Host, out: &mut Out) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        match ev {
            Event::Load => refresh(&mut s, host, out, None),
            Event::Command { name, args } => command(&mut s, host, out, name, args),
            Event::IoDone {
                handle,
                status,
                bytes,
            } => {
                let Some(at) = s.pending.iter().position(|p| p.0 == handle) else {
                    return;
                };
                let (_, io) = s.pending.swap_remove(at);
                done(&mut s, host, out, io, status, bytes);
            }
            _ => return,
        }
        out.contribute_commands(&crate::panel::commands(&s.m));
        out.contribute(Kind::Ui, &crate::panel::ui(&s.m));
    });
}

fn issue(s: &mut State, out: &mut Out, handle: Result<u32, Code>, io: Io) -> bool {
    match handle {
        Ok(h) if s.pending.len() < PENDING_MAX => {
            s.pending.push((h, io));
            true
        }
        Ok(_) => {
            say(s, out, Level::Error, "pdk: too many pending requests");
            false
        }
        Err(c) => {
            let msg = format!(
                "pdk: I/O refused ({c}): grant read_project, write_project, spawn:ciel and spawn:schemify"
            );
            say(s, out, Level::Error, &msg);
            false
        }
    }
}

/// Logs `text` and shows it on the panel.
fn say(s: &mut State, out: &mut Out, level: Level, text: &str) {
    out.log_at(level, text);
    s.m.message = text.to_owned();
}

fn argv(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// `F V` from command args.
fn family_version(args: &str) -> Result<Fv, String> {
    let mut v = text::values(args);
    let f = v
        .need()
        .map_err(|_| "expected <family> <version>".to_owned())?
        .into_owned();
    let ver = v
        .need()
        .map_err(|_| "expected <family> <version>".to_owned())?
        .into_owned();
    if recipe(&f).is_none() {
        let known: Vec<&str> = RECIPES.iter().map(|r| r.family).collect();
        return Err(format!("unknown family `{f}` ({})", known.join(", ")));
    }
    if !valid_version(&ver) {
        return Err(format!("`{ver}` is not a version id (never `latest`)"));
    }
    Ok((f, ver))
}

fn command(s: &mut State, host: &Host, out: &mut Out, name: &str, args: &str) {
    let row = |prefix: &str| {
        name.strip_prefix(prefix)
            .and_then(|i| i.parse::<usize>().ok())
    };
    let busy = s.m.job.as_ref().is_some_and(|j| j.step != "failed");
    match name {
        "pdk.panel" => s.m.open = !s.m.open,
        "pdk.list_remote" => {
            let f = text::values(args)
                .next()
                .and_then(Result::ok)
                .map(|f| f.into_owned());
            list_remote(s, host, out, f.as_deref().unwrap_or(&s.m.family.clone()));
        }
        _ if name.starts_with("pdk.family.") => {
            let f = name["pdk.family.".len()..].to_owned();
            list_remote(s, host, out, &f);
        }
        "pdk.install" => match family_version(args) {
            Ok(fv) if !busy => install(s, host, out, fv),
            Ok(_) => say(s, out, Level::Warn, "pdk.install: an install is running"),
            Err(e) => say(s, out, Level::Error, &format!("pdk.install: {e}")),
        },
        _ if row("pdk.remote.install.").is_some() => {
            let k = row("pdk.remote.install.").expect("checked");
            match s.m.remote.get(k).cloned() {
                Some(v) if !busy => {
                    let f = s.m.family.clone();
                    install(s, host, out, (f, v));
                }
                Some(_) => say(s, out, Level::Warn, "pdk.install: an install is running"),
                None => {}
            }
        }
        "pdk.use" | "pdk.remove" => match family_version(args) {
            Ok(fv) => {
                let a = if name == "pdk.use" {
                    Action::Use(fv)
                } else {
                    Action::Remove(fv)
                };
                refresh(s, host, out, Some(a));
            }
            Err(e) => say(s, out, Level::Error, &format!("{name}: {e}")),
        },
        _ if row("pdk.row.use.").is_some() || row("pdk.row.remove.").is_some() => {
            let (i, is_use) = match row("pdk.row.use.") {
                Some(i) => (i, true),
                None => (row("pdk.row.remove.").expect("checked"), false),
            };
            if let Some(fv) = s.m.installs.get(i).cloned() {
                let a = if is_use {
                    Action::Use(fv)
                } else {
                    Action::Remove(fv)
                };
                refresh(s, host, out, Some(a));
            }
        }
        "pdk.prune" => {
            let confirm = args.trim() == "confirm";
            refresh(s, host, out, Some(Action::Prune { confirm }));
        }
        "pdk.prune.confirm" => refresh(s, host, out, Some(Action::Prune { confirm: true })),
        _ => {}
    }
}

fn refresh(s: &mut State, host: &Host, out: &mut Out, after: Option<Action>) {
    s.after = after;
    for (path, io) in [
        (INDEX, Io::Index),
        ("schemify.project", Io::Manifest),
        ("schemify.lock", Io::Lock),
    ] {
        if issue(s, out, host.fs_read(path), io) {
            s.refreshing += 1;
        }
    }
}

fn list_remote(s: &mut State, host: &Host, out: &mut Out, family: &str) {
    let Some(r) = recipe(family) else {
        say(
            s,
            out,
            Level::Error,
            &format!("pdk.list_remote: unknown family `{family}`"),
        );
        return;
    };
    s.m.family = family.to_owned();
    s.m.remote.clear();
    s.m.message = format!("listing {family} versions…");
    let a = crate::ls_remote_argv(r);
    issue(
        s,
        out,
        host.spawn(&argv(&a), b""),
        Io::ListRemote(family.to_owned()),
    );
}

fn install(s: &mut State, host: &Host, out: &mut Out, fv: Fv) {
    let dir = version_dir(&fv.0, &fv.1);
    s.m.job = Some(Job {
        family: fv.0,
        version: fv.1,
        step: "prepare",
        tail: Vec::new(),
    });
    s.m.message.clear();
    issue(
        s,
        out,
        host.fs_remove(&format!("{dir}/STATE")),
        Io::ClearState,
    );
}

fn fail(s: &mut State, host: &Host, out: &mut Out, what: &str, bytes: &[u8]) {
    let (step, family, version) = match &mut s.m.job {
        Some(j) => {
            let step = j.step;
            j.step = "failed";
            j.tail = tail(bytes);
            (step, j.family.clone(), j.version.clone())
        }
        None => ("?", String::new(), String::new()),
    };
    let last = tail(bytes).pop().unwrap_or_default();
    let msg = format!("pdk.install {family} {version}: {step} failed ({what}) {last}");
    say(s, out, Level::Error, msg.trim_end());
    refresh(s, host, out, None);
}

/// Advances the install to `step`, issuing `handle` for it.
fn step(
    s: &mut State,
    out: &mut Out,
    name: &'static str,
    bytes: &[u8],
    handle: Result<u32, Code>,
    io: Io,
) {
    if let Some(j) = &mut s.m.job {
        j.step = name;
        if !bytes.is_empty() {
            j.tail = tail(bytes);
        }
    }
    issue(s, out, handle, io);
}

fn job_fv(s: &State) -> Option<Fv> {
    s.m.job
        .as_ref()
        .map(|j| (j.family.clone(), j.version.clone()))
}

fn done(s: &mut State, host: &Host, out: &mut Out, io: Io, status: i32, bytes: &[u8]) {
    let ok = status >= 0;
    match io {
        Io::Index | Io::Manifest | Io::Lock | Io::State(_) => {
            match io {
                Io::Index => {
                    s.m.installs = if ok {
                        parse_index(&String::from_utf8_lossy(bytes))
                    } else {
                        Vec::new()
                    };
                    s.m.complete = vec![None; s.m.installs.len()];
                    for fv in s.m.installs.clone() {
                        let path = format!("{}/STATE", version_dir(&fv.0, &fv.1));
                        if issue(s, out, host.fs_read(&path), Io::State(fv)) {
                            s.refreshing += 1;
                        }
                    }
                }
                Io::Manifest => s.manifest = ok.then(|| bytes.to_vec()),
                Io::Lock => s.lock = ok.then(|| bytes.to_vec()),
                Io::State(fv) => {
                    if let Some(i) = s.m.installs.iter().position(|x| *x == fv) {
                        s.m.complete[i] = Some(ok && bytes.trim_ascii() == b"ok");
                    }
                }
                _ => unreachable!("matched above"),
            }
            s.refreshing = s.refreshing.saturating_sub(1);
            if s.refreshing == 0 {
                refreshed(s, host, out);
            }
        }
        Io::ListRemote(f) => {
            if f != s.m.family {
                return;
            }
            if status != 0 {
                let last = tail(bytes).pop().unwrap_or_default();
                say(
                    s,
                    out,
                    Level::Error,
                    &format!("ciel ls-remote {f}: exit {status} {last}"),
                );
                return;
            }
            s.m.remote = parse_versions(&String::from_utf8_lossy(bytes));
            s.m.message = format!("{} {f} versions", s.m.remote.len());
        }
        // Install chain.
        Io::ClearState => {
            let Some(fv) = job_fv(s) else { return };
            let mut rows = s.m.installs.clone();
            rows.push(fv);
            let text = write_index(&rows);
            step(
                s,
                out,
                "prepare",
                b"",
                host.fs_write(INDEX, text.as_bytes()),
                Io::InstallIndex,
            );
        }
        Io::InstallIndex if !ok => fail(s, host, out, "cannot write the install index", bytes),
        Io::InstallIndex => {
            let Some((f, v)) = job_fv(s) else { return };
            let a = crate::fetch_argv(recipe(&f).expect("validated"), &v);
            step(
                s,
                out,
                "fetch (ciel)",
                b"",
                host.spawn(&argv(&a), b""),
                Io::Fetch,
            );
        }
        Io::Fetch if status != 0 => fail(s, host, out, &format!("ciel exit {status}"), bytes),
        Io::Fetch => {
            let Some((f, v)) = job_fv(s) else { return };
            let a = crate::import_argv(recipe(&f).expect("validated"), &v);
            step(
                s,
                out,
                "import (schemify)",
                bytes,
                host.spawn(&argv(&a), b""),
                Io::Import,
            );
        }
        Io::Import if status != 0 => fail(
            s,
            host,
            out,
            &format!("schemify import exit {status}"),
            bytes,
        ),
        Io::Import => {
            let Some((f, v)) = job_fv(s) else { return };
            let map = recipe(&f).expect("validated").map;
            let path = format!("{}/tech.map", version_dir(&f, &v));
            step(
                s,
                out,
                "tech.map",
                bytes,
                host.fs_write(&path, map.as_bytes()),
                Io::Map,
            );
        }
        Io::Map if !ok => fail(s, host, out, "cannot write tech.map", bytes),
        Io::Map => {
            let Some((f, v)) = job_fv(s) else { return };
            let path = format!("{}/STATE", version_dir(&f, &v));
            step(
                s,
                out,
                "state",
                b"",
                host.fs_write(&path, b"ok\n"),
                Io::StateOk,
            );
        }
        Io::StateOk if !ok => fail(s, host, out, "cannot write STATE", bytes),
        Io::StateOk => {
            let Some((f, v)) = job_fv(s) else { return };
            s.m.job = None;
            say(
                s,
                out,
                Level::Info,
                &format!("pdk: installed {f} {v} (pdk.use {f} {v} to switch to it)"),
            );
            refresh(s, host, out, None);
        }
        // Use chain.
        Io::UseImport(fv) if !ok => {
            let msg = format!(
                "pdk.use {} {}: no import manifest (install it again)",
                fv.0, fv.1
            );
            say(s, out, Level::Error, &msg);
        }
        Io::UseImport(fv) => {
            let manifest = s
                .manifest
                .clone()
                .unwrap_or_else(|| EMPTY_MANIFEST.to_vec());
            let r = recipe(&fv.0).expect("validated");
            match use_manifest(&manifest, bytes, r, &fv.1) {
                Ok(text) => {
                    issue(
                        s,
                        out,
                        host.fs_write("schemify.project", text.as_bytes()),
                        Io::UseWrite(fv),
                    );
                }
                Err(e) => say(s, out, Level::Error, &format!("pdk.use: {e}")),
            }
        }
        Io::UseWrite(fv) if !ok => say(
            s,
            out,
            Level::Error,
            &format!("pdk.use {} {}: cannot write schemify.project", fv.0, fv.1),
        ),
        Io::UseWrite(fv) => {
            say(
                s,
                out,
                Level::Info,
                &format!("pdk: using {} {} (project reloaded)", fv.0, fv.1),
            );
            refresh(s, host, out, None);
        }
        // Removal batch.
        Io::Remove(fv) => {
            let gone = status == Code::Gone.neg();
            if ok || gone {
                s.removed.push(fv);
            } else {
                let msg = format!(
                    "pdk.remove {} {}: {}",
                    fv.0,
                    fv.1,
                    String::from_utf8_lossy(bytes)
                );
                say(s, out, Level::Error, &msg);
            }
            s.removing = s.removing.saturating_sub(1);
            if s.removing == 0 {
                let removed = std::mem::take(&mut s.removed);
                let rows: Vec<Fv> =
                    s.m.installs
                        .iter()
                        .filter(|x| !removed.contains(x))
                        .cloned()
                        .collect();
                let list: Vec<String> = removed.iter().map(|(f, v)| format!("{f} {v}")).collect();
                s.m.message = format!("removed {}", list.join(", "));
                out.log(&format!("pdk: {}", s.m.message));
                let text = write_index(&rows);
                issue(
                    s,
                    out,
                    host.fs_write(INDEX, text.as_bytes()),
                    Io::RemoveIndex,
                );
            }
        }
        Io::RemoveIndex => refresh(s, host, out, None),
    }
}

/// A refresh landed: recompute references, then run the action waiting for it.
fn refreshed(s: &mut State, host: &Host, out: &mut Out) {
    let manifest = s
        .manifest
        .clone()
        .unwrap_or_else(|| EMPTY_MANIFEST.to_vec());
    match references(&manifest, s.lock.as_deref()) {
        Ok((active, locked)) => {
            s.m.active = active;
            s.m.locked = locked;
        }
        Err(e) => say(s, out, Level::Error, &format!("pdk: {e}")),
    }
    match s.after.take() {
        None => {}
        Some(Action::Use(fv)) => {
            let complete =
                s.m.installs
                    .iter()
                    .position(|x| *x == fv)
                    .and_then(|i| s.m.complete[i]);
            if complete != Some(true) {
                let msg = format!(
                    "pdk.use {} {}: refused: not installed (STATE is not ok)",
                    fv.0, fv.1
                );
                say(s, out, Level::Error, &msg);
                return;
            }
            let path = format!("{}/lib/schemify.project", version_dir(&fv.0, &fv.1));
            issue(s, out, host.fs_read(&path), Io::UseImport(fv));
        }
        Some(Action::Remove(fv)) => match remove_refusal(&fv, &s.m.active, &s.m.locked) {
            Some(why) => say(
                s,
                out,
                Level::Error,
                &format!("pdk.remove {} {}: refused: {why}", fv.0, fv.1),
            ),
            None => remove_batch(s, host, out, vec![fv]),
        },
        Some(Action::Prune { confirm }) => {
            let plan: Vec<Fv> = prune_plan(&s.m.installs, &s.m.active, &s.m.locked)
                .into_iter()
                .map(|i| s.m.installs[i].clone())
                .collect();
            match (confirm, s.m.prune.take()) {
                // Remove only what was listed and is still unreferenced.
                (true, Some(listed)) => {
                    let go: Vec<Fv> = plan.into_iter().filter(|x| listed.contains(x)).collect();
                    if go.is_empty() {
                        s.m.message = "nothing to prune".to_owned();
                    } else {
                        remove_batch(s, host, out, go);
                    }
                }
                _ => {
                    let list: Vec<String> = plan.iter().map(|(f, v)| format!("{f} {v}")).collect();
                    let msg = if list.is_empty() {
                        "pdk.prune: nothing to remove".to_owned()
                    } else {
                        format!(
                            "pdk.prune: would remove {}; run pdk.prune confirm",
                            list.join(", ")
                        )
                    };
                    say(s, out, Level::Info, &msg);
                    s.m.prune = Some(plan);
                }
            }
        }
    }
}

fn remove_batch(s: &mut State, host: &Host, out: &mut Out, list: Vec<Fv>) {
    assert!(s.removing == 0 && !list.is_empty());
    for fv in list {
        let dir = version_dir(&fv.0, &fv.1);
        if issue(s, out, host.fs_remove(&dir), Io::Remove(fv)) {
            s.removing += 1;
        }
    }
}
