//! The built guest (`plugins/interop/interop.wasm`, refreshed by `plugins/build.sh`) end to
//! end through the real host (`schemify_plugin`, wasmi, default fuel): the File menu and
//! commands compose without faults; a command without arguments opens the path panel; the
//! Virtuoso export, xschem export and SKILL install read through inverted I/O and write
//! byte-identically to the native job (`interop::job`, what the CLI runs). Skips when the
//! module has not been built.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use interop::job::{Command, Job, Step};
use interop::virtuoso::ExportOptions;
use schemify_plugin::abi::{Code, Event, FUEL_DEFAULT, Kind};
use schemify_plugin::{IoOp, PluginEvents, PluginId, Plugins, RequestQueues, Snapshots, deliver};

struct World {
    plugins: Plugins,
    events: PluginEvents,
    snap: Snapshots,
    queues: RequestQueues,
    id: PluginId,
}

impl World {
    fn new() -> Option<World> {
        let wasm =
            std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("interop.wasm")).ok()?;
        let mut plugins = Plugins::new();
        let id = plugins
            .load(
                "interop",
                &wasm,
                &["read_project", "write_project"],
                FUEL_DEFAULT,
            )
            .expect("loads");
        let mut w = World {
            plugins,
            events: PluginEvents::new(),
            snap: Snapshots::new(),
            queues: RequestQueues::default(),
            id,
        };
        w.frame();
        Some(w)
    }

    fn frame(&mut self) {
        deliver(
            &mut self.plugins,
            &self.events,
            &mut self.snap,
            &mut self.queues,
        );
        self.events.clear();
    }

    /// Runs `command` with `args`, serving reads from `files`; returns the writes.
    fn run(
        &mut self,
        command: &str,
        args: &str,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> BTreeMap<String, Vec<u8>> {
        std::mem::take(&mut self.queues);
        self.events
            .push(
                Some(self.id),
                &Event::Command {
                    name: command,
                    args,
                },
            )
            .expect("queued");
        self.frame();
        let mut written = BTreeMap::new();
        for _ in 0..10_000 {
            let q = std::mem::take(&mut self.queues);
            if q.io.is_empty() {
                break;
            }
            for r in &q.io {
                let path = r.args[0].clone();
                let (status, bytes): (i32, Vec<u8>) = match r.op {
                    IoOp::FsRead => match files.get(&path) {
                        Some(b) => (0, b.clone()),
                        None => (Code::Gone.neg(), Vec::new()),
                    },
                    IoOp::FsWrite => {
                        written.insert(path, r.data.clone());
                        (0, Vec::new())
                    }
                    op => panic!("unexpected {op:?}"),
                };
                self.events
                    .push(
                        Some(self.id),
                        &Event::IoDone {
                            handle: r.handle,
                            status,
                            bytes: &bytes,
                        },
                    )
                    .expect("queued");
            }
            self.frame();
        }
        let faults: Vec<String> = self
            .plugins
            .health()
            .faults()
            .iter()
            .map(|f| format!("{f:?}"))
            .collect();
        assert!(faults.is_empty(), "{faults:?}");
        written
    }
}

fn fixture(rel: &str) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, d: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let mut e: Vec<PathBuf> = std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        e.sort();
        for p in e {
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let r = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(r, std::fs::read(&p).unwrap());
            }
        }
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    let mut out = BTreeMap::new();
    walk(&dir, &dir, &mut out);
    out
}

/// The native job over the same files: what the CLI writes.
fn native(cmd: Command, files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    let mut j = Job::new(cmd);
    loop {
        match j.step() {
            Step::Read(ps) => {
                for p in ps {
                    let b = files.get(&p).cloned();
                    j.supply(&p, b);
                }
            }
            Step::Done { writes, .. } => return writes.into_iter().collect(),
            Step::Failed(p) => panic!("{p:?}"),
        }
    }
}

#[test]
fn contributes_the_file_menu_and_opens_the_path_panel() {
    let Some(mut w) = World::new() else {
        eprintln!("interop.wasm not built (plugins/build.sh); skipped");
        return;
    };
    let c = w.plugins.composed();
    for cmd in [
        "interop.virtuoso_export",
        "interop.virtuoso_import",
        "interop.xschem_export",
        "interop.install_skill",
    ] {
        assert!(
            c.of(Kind::Ui).iter().any(|r| r.text.contains(cmd)),
            "{cmd} in the menu"
        );
        assert!(
            c.of(Kind::Commands).iter().any(|r| r.text.contains(cmd)),
            "{cmd} registered"
        );
    }
    w.events
        .push(
            Some(w.id),
            &Event::Command {
                name: "interop.virtuoso_export",
                args: "",
            },
        )
        .unwrap();
    w.frame();
    assert!(
        w.plugins
            .composed()
            .of(Kind::Ui)
            .iter()
            .any(|r| r.text.contains("text_input"))
    );
}

#[test]
fn guest_conversions_equal_the_native_job_byte_for_byte() {
    let Some(mut w) = World::new() else {
        eprintln!("interop.wasm not built (plugins/build.sh); skipped");
        return;
    };
    let files = fixture("crates/app/tests/fixtures/inv");
    let got = w.run("interop.virtuoso_export", "top.shcomp out/inv.sfyx", &files);
    let want = native(
        Command::VirtuosoExport {
            tops: vec!["top.shcomp".into()],
            out: "out/inv.sfyx".into(),
            opts: ExportOptions {
                producer: "schemify interop plugin".into(),
                ..ExportOptions::default()
            },
        },
        &files,
    );
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        want.keys().collect::<Vec<_>>()
    );
    assert_eq!(got, want, "Virtuoso export: wasm ≠ native");
    let got = w.run("interop.xschem_export", "top.shcomp xs", &files);
    let want = native(
        Command::XschemExport {
            tops: vec!["top.shcomp".into()],
            out_dir: "xs".into(),
        },
        &files,
    );
    assert_eq!(got, want, "xschem export: wasm ≠ native");
    let got = w.run("interop.install_skill", "il", &files);
    assert_eq!(
        got["il/schemify.il"],
        interop::skill::schemify_il().into_bytes()
    );
    // A Virtuoso import of the export, into the same project.
    let mut with = files.clone();
    with.insert("out/inv.sfyx".into(), want_sfyx(&files));
    let got = w.run("interop.virtuoso_import", "out/inv.sfyx", &with);
    let want = native(
        Command::VirtuosoImport {
            sfyx: "out/inv.sfyx".into(),
            opts: Default::default(),
        },
        &with,
    );
    assert_eq!(got, want, "Virtuoso import: wasm ≠ native");
}

fn want_sfyx(files: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    native(
        Command::VirtuosoExport {
            tops: vec!["top.shcomp".into()],
            out: "out/inv.sfyx".into(),
            opts: ExportOptions {
                producer: "schemify interop plugin".into(),
                ..ExportOptions::default()
            },
        },
        files,
    )["out/inv.sfyx"]
        .clone()
}
