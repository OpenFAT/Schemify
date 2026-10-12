//! The built guest (`plugins/pdk-manager/pdk_manager.wasm`, from `plugins/build.sh`) through the
//! real host (`schemify_plugin`: grants and path checks included), with a fake `ciel` and a
//! fake `schemify` first on `PATH` (shell scripts that write a tiny tree and log their argv).
//! Filesystem and canonical-text oracles: the argv of every step, the store layout, `STATE`,
//! `tech.map`, the manifest after `pdk.use`, that a failing step leaves no `STATE ok`, that
//! `pdk.remove` refuses the active and the locked version, and that `pdk.prune` removes exactly
//! the unreferenced ones. Skips when the module has not been built.
//!
//! `real_ciel_installs_sky130` (ignored, nightly-style) runs the real `ciel` and `schemify` on
//! `PATH` against the network: `cargo test -p pdk-manager -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::Command;

use schemify_plugin::abi::{Code, Event, FUEL_DEFAULT, Kind};
use schemify_plugin::{IoOp, PluginEvents, PluginId, Plugins, RequestQueues, Snapshots, deliver};

const GRANTS: [&str; 4] = [
    "read_project",
    "write_project",
    "spawn:ciel",
    "spawn:schemify",
];

/// The fake `ciel`: logs its argv; `ls-remote` prints two versions; `enable` builds
/// `<root>/<variant>/libs.tech/xschem/xschemrc`, or fails for `$FAIL_CIEL`.
const FAKE_CIEL: &str = r#"#!/bin/sh
echo "ciel $*" >> "$FAKE_LOG"
case "$1" in
ls-remote)
  echo "Pre-built $3 PDK versions"
  echo "├── 1111111111111111111111111111111111111111 (2024.01.01)"
  echo "└── 2222222222222222222222222222222222222222 (2023.12.01)"
  exit 0 ;;
enable)
  root="$5"; eval "ver=\${$#}"
  if [ "$ver" = "$FAIL_CIEL" ]; then echo "downloading $ver"; echo "error: network unreachable" >&2; exit 3; fi
  case "$3" in sky130) v=sky130A ;; gf180mcu) v=gf180mcuD ;; *) v="$3" ;; esac
  mkdir -p "$root/$v/libs.tech/xschem" && echo "set PDK_ROOT x" > "$root/$v/libs.tech/xschem/xschemrc"
  echo "Version $ver enabled for the $3 PDK."
  exit 0 ;;
esac
exit 64
"#;

/// The fake `schemify import`: logs its argv; writes `<out>/schemify.project` and one cell, or
/// fails when the output directory names `$FAIL_IMPORT`.
const FAKE_SCHEMIFY: &str = r#"#!/bin/sh
echo "schemify $*" >> "$FAKE_LOG"
out="$4"
case "$out" in *"/$FAIL_IMPORT/"*) echo "import: no symbols found" >&2; exit 2 ;; esac
mkdir -p "$out/sky130_fd_pr"
printf 'schemify 1 project\n\nlib sky130_fd_pr path sky130_fd_pr\n\nset SKYWATER_MODELS ${PDK_ROOT}/${PDK}/libs.tech/combined\n' > "$out/schemify.project"
printf 'schemify 1 shprim\n' > "$out/sky130_fd_pr/nfet_01v8.shprim"
echo "imported 1 cell"
"#;

struct World {
    plugins: Plugins,
    events: PluginEvents,
    snap: Snapshots,
    queues: RequestQueues,
    id: PluginId,
    dir: PathBuf,
    path: String,
    log: PathBuf,
    env: Vec<(&'static str, String)>,
    /// Writes of `schemify.project` (each one makes the app run `project.reload`).
    reloads: usize,
}

impl World {
    fn new(tag: &str) -> Option<World> {
        let wasm =
            std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("pdk_manager.wasm")).ok()?;
        assert!(wasm.len() < 200 * 1024, "size limit: {}", wasm.len());
        let dir = std::env::temp_dir().join(format!("schemify-pdk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join(".fakebin");
        std::fs::create_dir_all(&bin).unwrap();
        for (name, body) in [("ciel", FAKE_CIEL), ("schemify", FAKE_SCHEMIFY)] {
            std::fs::write(bin.join(name), body).unwrap();
            let ok = Command::new("chmod")
                .arg("+x")
                .arg(bin.join(name))
                .status()
                .unwrap();
            assert!(ok.success());
        }
        std::fs::write(
            dir.join("schemify.project"),
            "schemify 1 project\n\nlib mine path lib\n",
        )
        .unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Some(World::with(wasm, dir.clone(), path, dir.join(".fakelog")))
    }

    fn with(wasm: Vec<u8>, dir: PathBuf, path: String, log: PathBuf) -> World {
        let mut plugins = Plugins::new();
        let id = plugins
            .load("pdk-manager", &wasm, &GRANTS, FUEL_DEFAULT)
            .expect("loads");
        let mut w = World {
            plugins,
            events: PluginEvents::new(),
            snap: Snapshots::new(),
            queues: RequestQueues::default(),
            id,
            dir,
            path,
            log,
            env: Vec::new(),
            reloads: 0,
        };
        w.run();
        w
    }

    /// Delivers, then answers I/O until the plugin has nothing outstanding.
    fn run(&mut self) {
        for _ in 0..1000 {
            deliver(
                &mut self.plugins,
                &self.events,
                &mut self.snap,
                &mut self.queues,
            );
            self.events.clear();
            let q = std::mem::take(&mut self.queues);
            if q.io.is_empty() {
                return;
            }
            for r in &q.io {
                let (status, bytes) = self.io(r.op, &r.args, &r.data);
                let ev = Event::IoDone {
                    handle: r.handle,
                    status,
                    bytes: &bytes,
                };
                self.events.push(Some(self.id), &ev).expect("queued");
            }
        }
        panic!("the plugin never settled");
    }

    /// The app's execution of an authorized request (`crates/app` `run_io`, spawn in the
    /// project root with the fake binaries first on `PATH`).
    fn io(&mut self, op: IoOp, args: &[String], data: &[u8]) -> (i32, Vec<u8>) {
        let gone = Code::Gone.neg();
        let path = self.dir.join(args.first().map_or("", String::as_str));
        match op {
            IoOp::FsRead => std::fs::read(&path).map_or((gone, Vec::new()), |b| (0, b)),
            IoOp::FsWrite => {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, data).unwrap();
                self.reloads += usize::from(args[0] == "schemify.project");
                (0, Vec::new())
            }
            IoOp::FsRemove if path.is_dir() => (
                std::fs::remove_dir_all(&path).map_or(-1, |()| 0),
                Vec::new(),
            ),
            IoOp::FsRemove if path.exists() => {
                (std::fs::remove_file(&path).map_or(-1, |()| 0), Vec::new())
            }
            IoOp::FsRemove => (gone, Vec::new()),
            IoOp::Spawn => {
                let mut c = Command::new(&args[0]);
                c.args(&args[1..])
                    .current_dir(&self.dir)
                    .env("PATH", &self.path)
                    .env("FAKE_LOG", &self.log);
                for (k, v) in &self.env {
                    c.env(k, v);
                }
                let o = c.output().expect("spawns");
                let mut bytes = o.stdout;
                bytes.extend_from_slice(&o.stderr);
                (o.status.code().unwrap_or(-1), bytes)
            }
            op => panic!("unexpected {op:?}"),
        }
    }

    fn cmd(&mut self, name: &str, args: &str) {
        self.events
            .push(Some(self.id), &Event::Command { name, args })
            .expect("queued");
        self.run();
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(rel)).ok()
    }

    fn take_log(&self) -> Vec<String> {
        let t = std::fs::read_to_string(&self.log).unwrap_or_default();
        let _ = std::fs::remove_file(&self.log);
        t.lines().map(str::to_owned).collect()
    }

    fn ui(&self) -> String {
        let c = self.plugins.composed();
        c.of(Kind::Ui)
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn faults(&self) -> Vec<String> {
        self.plugins
            .health()
            .faults()
            .iter()
            .map(|f| format!("{f:?}"))
            .collect()
    }
}

const V1: &str = "1111111111111111111111111111111111111111";
const V2: &str = "2222222222222222222222222222222222222222";

#[test]
fn install_use_remove_prune() {
    let Some(mut w) = World::new("flow") else {
        eprintln!("pdk_manager.wasm not built (plugins/build.sh); skipped");
        return;
    };
    w.env = vec![
        ("FAIL_CIEL", "v-bad-fetch".into()),
        ("FAIL_IMPORT", "v-bad-import".into()),
    ];
    assert!(w.ui().contains("node view.pdks item"), "the View menu item");
    // The panel and the remote listing.
    w.cmd("pdk.panel", "");
    w.cmd("pdk.family.sky130", "");
    assert_eq!(w.take_log(), ["ciel ls-remote --pdk-family sky130"]);
    assert!(
        w.ui()
            .contains("node pdk.remote.1.name label\n  parent pdk.remote.1\n  label 2222222"),
        "{}",
        w.ui()
    );

    // Install V1 from the panel row: exact argv, store layout, STATE, tech.map.
    w.cmd("pdk.remote.install.0", "");
    let d = format!(".schemify/pdks/sky130/{V1}");
    assert_eq!(
        w.take_log(),
        [
            format!("ciel enable --pdk-family sky130 --pdk-root {d}/root -l sky130_fd_pr {V1}"),
            format!(
                "schemify import {d}/root/sky130A/libs.tech/xschem -o {d}/lib --xschemrc {d}/root/sky130A/libs.tech/xschem/xschemrc"
            ),
        ]
    );
    assert_eq!(w.read(&format!("{d}/STATE")).as_deref(), Some("ok\n"));
    assert_eq!(
        w.read(&format!("{d}/tech.map")).as_deref(),
        Some(include_str!("../maps/sky130.map"))
    );
    assert!(
        w.dir
            .join(format!("{d}/root/sky130A/libs.tech/xschem/xschemrc"))
            .exists()
    );
    assert!(
        w.dir
            .join(format!("{d}/lib/sky130_fd_pr/nfet_01v8.shprim"))
            .exists()
    );
    assert_eq!(
        w.read(".schemify/pdks/INSTALLS").unwrap(),
        format!("sky130 {V1}\n")
    );

    // Failing steps leave no `STATE ok` (and stop the chain).
    w.cmd("pdk.install", "sky130 v-bad-fetch");
    assert_eq!(w.take_log().len(), 1, "import never ran after ciel failed");
    assert!(w.read(".schemify/pdks/sky130/v-bad-fetch/STATE").is_none());
    w.cmd("pdk.install", "sky130 v-bad-import");
    assert_eq!(w.take_log().len(), 2);
    assert!(w.read(".schemify/pdks/sky130/v-bad-import/STATE").is_none());
    assert!(
        w.read(".schemify/pdks/sky130/v-bad-import/tech.map")
            .is_none()
    );
    assert!(w.ui().contains("label incomplete"));
    assert!(
        w.ui().contains("no symbols found"),
        "the failing step's output tail: {}",
        w.ui()
    );
    // A version that was ok and is re-installed with a failure loses its `ok`.
    w.cmd("pdk.install", &format!("sky130 {V2}"));
    assert_eq!(
        w.read(&format!(".schemify/pdks/sky130/{V2}/STATE"))
            .as_deref(),
        Some("ok\n")
    );
    w.env[0].1 = V2.into();
    w.cmd("pdk.install", &format!("sky130 {V2}"));
    assert!(
        w.read(&format!(".schemify/pdks/sky130/{V2}/STATE"))
            .is_none()
    );
    w.env[0].1 = "v-bad-fetch".into();
    w.cmd("pdk.install", &format!("sky130 {V2}"));
    w.take_log();

    // `pdk.use` refuses an incomplete install, then rewrites the manifest (canonical text).
    w.cmd("pdk.use", "sky130 v-bad-import");
    assert_eq!(w.reloads, 0);
    w.cmd("pdk.use", &format!("sky130 {V1}"));
    assert_eq!(w.reloads, 1, "one manifest write → one project.reload");
    assert_eq!(
        w.read("schemify.project").unwrap(),
        format!(
            "schemify 1 project\n\nlib mine path lib\nlib sky130_fd_pr path {d}/lib/sky130_fd_pr\nlib tech map {d}/tech.map\n\nset PDK_ROOT {d}/root\nset PDK sky130A\nset PDK_MODELS ${{PDK_ROOT}}/sky130A/libs.tech/combined/sky130.lib.spice\nset SKYWATER_MODELS ${{PDK_ROOT}}/${{PDK}}/libs.tech/combined\n"
        )
    );
    assert!(w.ui().contains("label active"));

    // `pdk.remove` refuses the active and the locked version; removes a free one.
    std::fs::write(
        w.dir.join("schemify.lock"),
        format!("schemify 1 lock\nlocked lib sky130_fd_pr path {}/.schemify/pdks/sky130/{V2}/lib/sky130_fd_pr 00\n", w.dir.display()),
    )
    .unwrap();
    w.cmd("pdk.remove", &format!("sky130 {V1}"));
    assert!(w.dir.join(&d).exists(), "active: refused");
    w.cmd("pdk.remove", &format!("sky130 {V2}"));
    assert!(
        w.dir.join(format!(".schemify/pdks/sky130/{V2}")).exists(),
        "locked: refused"
    );
    assert!(w.ui().contains("pinned by schemify.lock"));

    // `pdk.prune` lists, then removes exactly the unreferenced installs (complete or not).
    w.cmd("pdk.install", "gf180mcu spare");
    let before = w.read(".schemify/pdks/INSTALLS").unwrap();
    w.cmd("pdk.prune", "");
    assert_eq!(
        w.read(".schemify/pdks/INSTALLS").unwrap(),
        before,
        "listing only"
    );
    assert!(
        w.ui()
            .contains("Removes 3: gf180mcu spare, sky130 v-bad-f, sky130 v-bad-i"),
        "{}",
        w.ui()
    );
    w.cmd("pdk.prune", "confirm");
    for gone in [
        "gf180mcu/spare",
        "sky130/v-bad-fetch",
        "sky130/v-bad-import",
    ] {
        assert!(
            !w.dir.join(format!(".schemify/pdks/{gone}")).exists(),
            "{gone}"
        );
    }
    assert!(w.dir.join(&d).exists() && w.dir.join(format!(".schemify/pdks/sky130/{V2}")).exists());
    assert_eq!(
        w.read(".schemify/pdks/INSTALLS").unwrap(),
        format!("sky130 {V1}\nsky130 {V2}\n")
    );
    // Switching technology: the previous family's records go.
    std::fs::remove_file(w.dir.join("schemify.lock")).unwrap();
    w.cmd("pdk.remove", &format!("sky130 {V2}"));
    assert!(!w.dir.join(format!(".schemify/pdks/sky130/{V2}")).exists());
    assert!(w.faults().is_empty(), "{:?}", w.faults());
}

#[test]
fn bad_arguments_change_nothing() {
    let Some(mut w) = World::new("args") else {
        return;
    };
    let manifest = w.read("schemify.project").unwrap();
    for (cmd, args) in [
        ("pdk.install", "sky130 latest"),
        ("pdk.install", "sky130 ../../x"),
        ("pdk.install", "asap7 v1"),
        ("pdk.install", "sky130"),
        ("pdk.use", "sky130 never-installed"),
        ("pdk.remove", "sky130 .."),
    ] {
        w.cmd(cmd, args);
        assert!(w.take_log().is_empty(), "{cmd} {args}: nothing spawned");
    }
    assert_eq!(w.read("schemify.project").unwrap(), manifest);
    assert!(!w.dir.join(".schemify/pdks/sky130").exists());
    assert_eq!(w.reloads, 0);
}

/// Nightly-style: the real `ciel` (network) and `schemify` on `PATH` install the newest sky130
/// into a scratch project, and the store holds an imported `sky130_fd_pr` library.
#[test]
#[ignore = "network and a real ciel: run with --ignored"]
fn real_ciel_installs_sky130() {
    let wasm = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("pdk_manager.wasm"))
        .expect("built");
    let dir = std::env::temp_dir().join(format!("schemify-pdk-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schemify.project"), "schemify 1 project\n").unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    let mut w = World::with(wasm, dir.clone(), path, dir.join(".log"));
    w.cmd("pdk.panel", "");
    w.cmd("pdk.family.sky130", "");
    let ui = w.ui();
    let v = ui
        .split("tooltip ")
        .filter_map(|t| t.split_whitespace().next())
        .find(|t| t.len() == 40)
        .expect("ciel ls-remote listed a version")
        .to_owned();
    w.cmd("pdk.install", &format!("sky130 {v}"));
    let d = format!(".schemify/pdks/sky130/{v}");
    assert_eq!(
        w.read(&format!("{d}/STATE")).as_deref(),
        Some("ok\n"),
        "{}",
        w.ui()
    );
    assert!(dir.join(format!("{d}/lib/sky130_fd_pr")).is_dir());
    w.cmd("pdk.use", &format!("sky130 {v}"));
    assert!(w.read("schemify.project").unwrap().contains("lib tech map"));
}
