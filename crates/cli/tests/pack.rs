//! The package manager with no network (config spec §5): local `path` sources, `git` sources
//! from a temp repository through file paths, `url` sources through `file://` with `curl`.
//! Oracles: the lockfile rows equal the commit and the sha256 of the module actually
//! installed; restore reproduces the locked bytes; grants are the intersection.

use std::path::{Path, PathBuf};
use std::process::Command;

use schemify_cli::pack::{self, Paths};
use schemify_config::{PackSpec, Source};
use schemify_doc::{hex, sha256};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("schemify-pack-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn paths(dir: &Path) -> Paths {
    Paths {
        pack: dir.join("data/pack"),
        lock: dir.join("config/schemify-pack.lock"),
    }
}

fn spec(name: &str, source: Source) -> PackSpec {
    PackSpec {
        name: name.into(),
        source,
        grant: Vec::new(),
        fuel: None,
        lazy_cmd: Vec::new(),
        lazy_event: Vec::new(),
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Lock rows as `(name, kind, resolved, hash)`.
fn lock_rows(p: &Paths) -> Vec<(String, String, String, String)> {
    let Ok(bytes) = std::fs::read(&p.lock) else {
        return Vec::new();
    };
    // Read through the format layer, as `pack` does: a Windows path is a quoted, escaped value.
    let project = schemify_doc::Project::new(None, Some(&bytes), 0).expect("lockfile parses");
    let l = project.lock();
    (0..l.name.len())
        .filter(|&i| l.ty[i] == "plugin")
        .map(|i| {
            let f = |c: &[String]| c[i].clone();
            (f(&l.name), f(&l.kind), f(&l.resolved), f(&l.hash))
        })
        .collect()
}

fn digest(bytes: &[u8]) -> String {
    hex(&sha256(bytes))
}

/// A repository with two commits of `<name>.wasm` (`a`, then `b`), `a` tagged `v1`.
fn repo(dir: &Path, name: &str) -> (PathBuf, String, String) {
    let r = dir.join("upstream");
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(r.join(format!("{name}.wasm")), b"module a").unwrap();
    git(&r, &["add", "."]);
    git(&r, &["commit", "--quiet", "-m", "a"]);
    git(&r, &["tag", "v1"]);
    let a = git(&r, &["rev-parse", "HEAD"]);
    std::fs::write(r.join(format!("{name}.wasm")), b"module b").unwrap();
    git(&r, &["commit", "--quiet", "-am", "b"]);
    let b = git(&r, &["rev-parse", "HEAD"]);
    (r, a, b)
}

#[test]
fn git_sync_lock_update_restore_round_trip() {
    let dir = scratch("git");
    let p = paths(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    let (r, a, b) = repo(&dir, "lut");
    let url = r.display().to_string();
    let pinned = spec(
        "lut",
        Source::Git {
            url: url.clone(),
            rev: Some("v1".into()),
        },
    );

    // sync installs the pinned tag and locks its commit and module hash.
    let rep = pack::sync(std::slice::from_ref(&pinned), &p);
    assert_eq!(rep.failed, 0, "{:?}", rep.lines);
    assert_eq!(
        lock_rows(&p),
        vec![("lut".into(), "git".into(), a.clone(), digest(b"module a"))]
    );
    let installed = p.pack.join("lut/lut.wasm");
    assert_eq!(std::fs::read(&installed).unwrap(), b"module a");

    // Unpinned, sync leaves an installed package alone; update moves it and the lock.
    let floating = spec("lut", Source::Git { url, rev: None });
    assert_eq!(pack::sync(std::slice::from_ref(&floating), &p).failed, 0);
    assert_eq!(
        lock_rows(&p)[0].2,
        a,
        "sync does not move an installed package"
    );
    let saved_lock = std::fs::read(&p.lock).unwrap();
    let rep = pack::update(std::slice::from_ref(&floating), &p);
    assert_eq!(rep.failed, 0, "{:?}", rep.lines);
    assert_eq!(
        lock_rows(&p),
        vec![("lut".into(), "git".into(), b.clone(), digest(b"module b"))]
    );
    assert_eq!(std::fs::read(&installed).unwrap(), b"module b");

    // restore goes back to whatever the lockfile says, also after a wipe.
    std::fs::write(&p.lock, &saved_lock).unwrap();
    let rep = pack::restore(std::slice::from_ref(&floating), &p);
    assert_eq!(rep.failed, 0, "{:?}", rep.lines);
    assert_eq!(std::fs::read(&installed).unwrap(), b"module a");
    std::fs::remove_dir_all(&p.pack).unwrap();
    assert_eq!(pack::restore(std::slice::from_ref(&floating), &p).failed, 0);
    assert_eq!(std::fs::read(&installed).unwrap(), b"module a");
    assert_eq!(
        std::fs::read(&p.lock).unwrap(),
        saved_lock,
        "restore keeps the lock"
    );
    let status = pack::status(std::slice::from_ref(&floating), &p);
    assert_eq!(status.failed, 0);
    assert!(status.lines[0].ends_with(" ok"), "{:?}", status.lines);

    // An unknown revision fails that package and leaves the lock alone.
    let bogus = spec(
        "lut",
        Source::Git {
            url: r.display().to_string(),
            rev: Some("v9".into()),
        },
    );
    let rep = pack::update(&[bogus], &p);
    assert_eq!(rep.failed, 1, "{:?}", rep.lines);
    assert_eq!(std::fs::read(&p.lock).unwrap(), saved_lock);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_sources_lock_their_hash_and_clean_removes_unlisted() {
    let dir = scratch("path");
    let p = paths(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    let local = dir.join("dev-plugin");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(local.join("dev.wasm"), b"dev v1").unwrap();
    let dev = spec("dev", Source::Path(local.clone()));
    let (r, a, _) = repo(&dir, "old");
    let old = spec(
        "old",
        Source::Git {
            url: r.display().to_string(),
            rev: Some(a),
        },
    );

    assert_eq!(pack::sync(&[dev.clone(), old], &p).failed, 0);
    let rows = lock_rows(&p);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        (
            "dev".into(),
            "path".into(),
            local.display().to_string(),
            digest(b"dev v1")
        )
    );
    assert!(p.pack.join("old").is_dir());
    assert!(
        !p.pack.join("dev").exists(),
        "a path package is used in place"
    );

    // A local change shows as `changed`; restore refuses it (a path cannot be re-fetched).
    std::fs::write(local.join("dev.wasm"), b"dev v2").unwrap();
    let s = pack::status(std::slice::from_ref(&dev), &p);
    assert!(s.lines[0].contains(" changed"), "{:?}", s.lines);
    assert_eq!(pack::restore(std::slice::from_ref(&dev), &p).failed, 1);

    // clean: `old` is no longer listed: its directory and lock row go.
    let rep = pack::clean(std::slice::from_ref(&dev), &p);
    assert_eq!(rep.failed, 0, "{:?}", rep.lines);
    assert_eq!(rep.lines, vec!["old: removed".to_owned()]);
    assert!(!p.pack.join("old").exists());
    assert_eq!(lock_rows(&p).len(), 1);
    // Cleaning everything deletes the lockfile.
    assert_eq!(pack::clean(&[], &p).failed, 0);
    assert!(!p.lock.exists());
    let s = pack::status(&[spec("ghost", Source::Path(dir.join("nope")))], &p);
    assert_eq!(s.failed, 1);
    assert!(s.lines[0].contains("missing"), "{:?}", s.lines);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn url_sources_verify_sha256_before_installing() {
    if Command::new("curl").arg("--version").output().is_err() {
        eprintln!("skipped: no curl on PATH");
        return;
    }
    let dir = scratch("url");
    let p = paths(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    let served = dir.join("served.wasm");
    std::fs::write(&served, b"remote module").unwrap();
    let url = format!("file://{}", served.display());
    let wrong = spec(
        "eye",
        Source::Url {
            url: url.clone(),
            sha256: "0".repeat(64),
        },
    );
    let rep = pack::sync(&[wrong], &p);
    assert_eq!(rep.failed, 1);
    assert!(rep.lines[0].contains("sha256 mismatch"), "{:?}", rep.lines);
    assert!(!p.pack.join("eye").exists(), "nothing installed");
    let leftovers: Vec<_> = std::fs::read_dir(&p.pack).unwrap().collect();
    assert!(
        leftovers.is_empty(),
        "the download was deleted: {leftovers:?}"
    );
    assert!(lock_rows(&p).is_empty());

    let good = spec(
        "eye",
        Source::Url {
            url: url.clone(),
            sha256: digest(b"remote module"),
        },
    );
    assert_eq!(pack::sync(std::slice::from_ref(&good), &p).failed, 0);
    assert_eq!(
        std::fs::read(p.pack.join("eye/eye.wasm")).unwrap(),
        b"remote module"
    );
    assert_eq!(
        lock_rows(&p),
        vec![("eye".into(), "url".into(), url, digest(b"remote module"))]
    );
    // restore re-fetches a missing file and checks it against the lock.
    std::fs::remove_dir_all(p.pack.join("eye")).unwrap();
    assert_eq!(pack::restore(std::slice::from_ref(&good), &p).failed, 0);
    assert!(p.pack.join("eye/eye.wasm").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grants_are_the_intersection_of_request_and_grant() {
    let dir = scratch("grants");
    let p = paths(&dir);
    let with_manifest = dir.join("m");
    std::fs::create_dir_all(&with_manifest).unwrap();
    std::fs::write(with_manifest.join("mod.wasm"), b"x").unwrap();
    std::fs::write(
        with_manifest.join(pack::MANIFEST),
        "schemify 1 project\n\nplugin m path mod.wasm\n  grant read_project\n  grant spawn:ngspice\n  fuel 2000000\n",
    )
    .unwrap();
    let mut s = spec("m", Source::Path(with_manifest));
    s.grant = vec!["read_project".into(), "network".into()];
    let got = pack::locate(&s, &p).unwrap();
    assert_eq!(got.wasm.file_name().unwrap(), "mod.wasm");
    assert_eq!(got.grants, vec!["read_project".to_owned()]);
    assert_eq!(got.denied, vec!["spawn:ngspice".to_owned()]);
    assert_eq!(got.unused, vec!["network".to_owned()]);
    assert_eq!(
        got.fuel,
        Some(2_000_000),
        "the manifest's fuel when the spec has none"
    );
    s.fuel = Some(5_000_000);
    assert_eq!(
        pack::locate(&s, &p).unwrap().fuel,
        Some(5_000_000),
        "the spec's fuel wins"
    );

    // No manifest: the module is <name>.wasm and it requests exactly what is granted.
    let bare = dir.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::write(bare.join("bare.wasm"), b"y").unwrap();
    let mut b = spec("bare", Source::Path(bare));
    b.grant = vec!["clipboard".into()];
    let got = pack::locate(&b, &p).unwrap();
    assert_eq!(got.grants, vec!["clipboard".to_owned()]);
    assert!(got.denied.is_empty() && got.unused.is_empty());
    // Not installed: a message naming the fix.
    let missing = spec("gone", Source::Path(dir.join("gone")));
    assert!(
        pack::locate(&missing, &p)
            .unwrap_err()
            .contains("schemify pack sync")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
