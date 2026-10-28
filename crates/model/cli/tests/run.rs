//! `run` and `publish` end to end against a fake simulator (a shell script standing in for
//! each `schemify.sim` row, run spec §5): the jobs each selection opens, the dialect each
//! simulator gets, a PDK's pinned simulator, the saved run's `bench` sidecar, and the hosted
//! layout `publish` writes from saved runs (web spec §3.1).

mod common;

use std::path::{Path, PathBuf};

use common::{schemify, scratch};

/// A bench whose two configs reach different cells: `beh` makes `amp` a leaf (no `res`),
/// `sch` (the default) reaches `res` through amp's schematic.
const AMP: &str = "schemify 1 shcomp\n  default sch\n\npin IN in 0 0\npin OUT out 0 20\n\n\
    impl sch schematic\nimpl beh spice\n  format spice \"R{name} {IN} {OUT} 2k\"\n\n\
    inst 0r0000 ./res\n  at 0 0 R0\n  name R1\n\nport 0p0001 0 0 R0 IN\nport 0p0002 0 20 R0 OUT\n";
const RES: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\n\
    impl spice spice\n  format spice \"R{name} {A} {B} 1k\"\n";
const BENCH: &str = "schemify 1 shbench\n\ninst 0x0001 ./amp\n  at 0 0 R0\n  name X1\n\n\
    label 0n0001 0 0 R0 a\nlabel 0n0002 0 20 R0 0\n\n\
    config beh\n  cell ./amp beh\nconfig sch\n  order sch\n\ncode op spice\n  | V1 a 0 1\n  | .op\n";

/// Writes an ASCII raw file (one operating point) for the netlist `$1` into `$2`; with
/// `$WANT` set, refuses a netlist whose header is not `(schemify $WANT)` and, with `$ZIG`
/// unset, an espice one.
const FAKE: &str = r#"if [ -n "$WANT" ]; then head -1 "$1" | grep -q "(schemify $WANT)" || { echo "not $WANT" >&2; exit 3; }; fi
if head -1 "$1" | grep -q espice && [ -z "$ZIG" ]; then echo 'no ZIG' >&2; exit 4; fi
printf 'Title: fake\nPlotname: Operating Point\nFlags: real\nNo. Variables: 1\nNo. Points: 1\nVariables:\n 0 v(a) voltage\nValues:\n0 1\n' > "$2"
"#;

// ---- private ----

fn run_project(name: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::write(dir.join("schemify.project"), "schemify 1 project\n").unwrap();
    std::fs::write(dir.join("amp.shcomp"), AMP).unwrap();
    std::fs::write(dir.join("res.shprim"), RES).unwrap();
    std::fs::write(dir.join("tb.shbench"), BENCH).unwrap();
    dir
}

/// `init.lua` lines naming the fake simulator for `sim` (with `extra` fields).
fn fake_sim(sim: &str, extra: &str) -> String {
    format!(
        "schemify.sim('{sim}', {{ path = 'sh', args = {{ '-c', [==[{FAKE}]==], 'fake', \
         '{{netlist}}', '{{raw}}' }}{extra} }})\n"
    )
}

/// Bug fix: `run` with several `--config`s opens the cells every config reaches, not only
/// the first's (the second config's job used to fail "cells not open").
#[test]
fn run_opens_the_cells_of_every_selected_config() {
    let dir = run_project("run-configs");
    std::fs::write(dir.join("init.lua"), fake_sim("ngspice", "")).unwrap();
    for order in [["beh", "sch"], ["sch", "beh"]] {
        let (code, text) = schemify(
            &dir,
            &[
                "-u",
                "init.lua",
                "run",
                "tb.shbench",
                "--config",
                order[0],
                "--config",
                order[1],
            ],
        );
        assert_eq!(code, 0, "{order:?}: {text}");
        assert_eq!(text.matches(" done\n").count(), 2, "{text}");
        assert!(!text.contains("not open"), "{text}");
    }
    // Negative space: a config the bench lacks still refuses the run.
    let (code, text) = schemify(
        &dir,
        &["-u", "init.lua", "run", "tb.shbench", "--config", "nope"],
    );
    assert_ne!(code, 0, "{text}");
}

/// The run directory `run` saves names its bench in a `bench` sidecar, as the editor's
/// runs do (`spines::bench_name`), so the editor's history offers it: `lib:stem` in a named
/// library, the bare stem in the project's unnamed one, whichever way the path was given.
#[test]
fn run_records_the_bench_name_the_editor_looks_for() {
    let sidecars = |dir: &Path| -> Vec<String> {
        let runs = dir.join(".schemify/runs");
        let mut v: Vec<String> = std::fs::read_dir(&runs)
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path().join("bench")).unwrap())
            .collect();
        v.sort();
        v
    };
    let dir = run_project("run-sidecar");
    std::fs::write(dir.join("init.lua"), fake_sim("ngspice", "")).unwrap();
    let (code, text) = schemify(&dir, &["-u", "init.lua", "run", "tb.shbench"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(sidecars(&dir), ["tb\n"]);

    // Each path form in its own project: one design shares one run directory, so a second
    // run in the same project would rewrite the first one's sidecar.
    for (name, abs) in [("run-sidecar-rel", false), ("run-sidecar-abs", true)] {
        let dir = scratch(name);
        let lib = dir.join("benches");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(
            dir.join("schemify.project"),
            "schemify 1 project\n\nlib benches path benches\n",
        )
        .unwrap();
        for (f, text) in [
            ("amp.shcomp", AMP),
            ("res.shprim", RES),
            ("tb.shbench", BENCH),
        ] {
            std::fs::write(lib.join(f), text).unwrap();
        }
        std::fs::write(dir.join("init.lua"), fake_sim("ngspice", "")).unwrap();
        let path = if abs {
            lib.join("tb.shbench").to_string_lossy().into_owned()
        } else {
            "benches/tb.shbench".to_owned()
        };
        let (code, text) = schemify(&dir, &["-u", "init.lua", "run", &path]);
        assert_eq!(code, 0, "{path}: {text}");
        assert_eq!(sidecars(&dir), ["benches:tb\n"], "{path}");
    }
}

/// `publish` lays a project out for the web editor (web spec §3.1): every listed file plus
/// the saved runs under `projects/<name>/`, a manifest whose hashes match the bytes, an
/// index linking each bench with its newest run; publishing again replaces the project
/// whole. Negative space: a directory without `schemify.project` is a usage error.
#[test]
fn publish_writes_a_hosted_project_with_its_runs() {
    let dir = run_project("publish-src");
    std::fs::write(dir.join("init.lua"), fake_sim("ngspice", "")).unwrap();
    let (code, text) = schemify(&dir, &["-u", "init.lua", "run", "tb.shbench"]);
    assert_eq!(code, 0, "{text}");
    let site = scratch("publish-site");
    let src = dir.to_string_lossy().into_owned();
    let out = site.to_string_lossy().into_owned();
    let (code, text) = schemify(&site, &["publish", &src, "-o", &out]);
    assert_eq!(code, 0, "{text}");
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    let proj = site.join("projects").join(&name);
    let manifest = std::fs::read_to_string(proj.join("schemify.files")).unwrap();
    let rows = schemify_cli::hosted::parse(&manifest).unwrap();
    for (rel, digest) in &rows {
        let bytes = std::fs::read(proj.join(rel)).unwrap();
        assert!(schemify_cli::hosted::matches(digest, &bytes), "{rel}");
    }
    let paths: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(paths.contains(&"tb.shbench") && paths.contains(&"schemify.project"));
    assert!(
        paths
            .iter()
            .any(|p| p.starts_with(".schemify/runs/") && p.ends_with("/results"))
    );
    assert!(paths.iter().any(|p| p.ends_with("/bench")), "{paths:?}");
    let index = std::fs::read_to_string(site.join("projects/index.html")).unwrap();
    assert!(
        index.contains(&format!("../#/projects/{name}/tb.shbench")),
        "{index}"
    );
    assert!(index.contains("1 job done"), "{index}");

    // Republish after a file went away: the site loses it too.
    std::fs::remove_file(dir.join("res.shprim")).unwrap();
    std::fs::write(proj.join("stale.txt"), "x").unwrap();
    assert_eq!(schemify(&site, &["publish", &src, "-o", &out]).0, 0);
    assert!(!proj.join("res.shprim").exists() && !proj.join("stale.txt").exists());

    let empty = scratch("publish-empty");
    let (code, _) = schemify(&site, &["publish", &empty.to_string_lossy(), "-o", &out]);
    assert_eq!(code, 2);
}

/// `run --dialect espice` and `schemify.opt.simulator = "espice"` netlist for espice and start
/// the `espice` sim row with its `env`; `netlist --dialect espice` writes the espice text;
/// a dialect no simulator reads is a usage error for `run`.
#[test]
fn run_and_netlist_select_espice() {
    let dir = run_project("run-espice");
    let init = fake_sim("espice", ", env = { ZIG = '/z/zig', WANT = 'espice' }")
        + &fake_sim("ngspice", ", env = { WANT = 'ngspice' }");
    std::fs::write(dir.join("init.lua"), &init).unwrap();
    let run = |args: &[&str]| {
        let mut all = vec!["-u", "init.lua", "run", "tb.shbench"];
        all.extend_from_slice(args);
        schemify(&dir, &all)
    };
    let (code, text) = run(&["--dialect", "espice"]);
    assert_eq!((code, text.matches(" done\n").count()), (0, 1), "{text}");
    let (code, text) = run(&[]);
    assert_eq!(code, 0, "ngspice by default: {text}");
    std::fs::write(
        dir.join("init.lua"),
        init.clone() + "schemify.opt.simulator = 'espice'\n",
    )
    .unwrap();
    let (code, text) = run(&[]);
    assert_eq!(code, 0, "the configured simulator: {text}");
    let (code, text) = run(&["--dialect", "ngspice"]);
    assert_eq!(code, 0, "--dialect wins: {text}");
    assert_eq!(run(&["--dialect", "cdl"]).0, 2);
    assert_eq!(run(&["--dialect", "bogus"]).0, 2);
    let (code, text) = schemify(&dir, &["netlist", "tb.shbench", "--dialect", "espice"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.starts_with("* tb (schemify espice)\n"), "{text}");
}

/// A PDK's pinned simulator (tech map `simulator`, run spec §5) outranks the user config and
/// yields to `--dialect`; a pin Schemify cannot start, or whose program is missing, refuses
/// the run naming the PDK instead of falling back; a pin naming no simulator is an error.
#[test]
fn a_pdk_pins_its_simulator() {
    let dir = run_project("run-pin");
    std::fs::write(
        dir.join("schemify.project"),
        "schemify 1 project\n\nlib tech map tech.map\n",
    )
    .unwrap();
    let pin = |word: &str| {
        let map = format!("schemify 1 techmap\n  pdk kit\n  simulator {word}\n");
        std::fs::write(dir.join("tech.map"), map).unwrap();
    };
    let init = fake_sim("espice", ", env = { ZIG = '/z/zig' }") + &fake_sim("ngspice", "");
    std::fs::write(
        dir.join("init.lua"),
        init.clone() + "schemify.opt.simulator = 'ngspice'\n",
    )
    .unwrap();
    let cli = |args: &[&str]| {
        let mut all = vec!["-u", "init.lua"];
        all.extend_from_slice(args);
        schemify(&dir, &all)
    };
    pin("espice");
    let (code, text) = cli(&["netlist", "tb.shbench"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.starts_with("* tb (schemify espice)\n"),
        "the pin outranks the config: {text}"
    );
    let (code, text) = cli(&["netlist", "tb.shbench", "--dialect", "ngspice"]);
    assert!(
        code == 0 && text.starts_with("* tb (schemify ngspice)\n"),
        "{text}"
    );
    let (code, text) = cli(&["run", "tb.shbench"]);
    assert_eq!((code, text.matches(" done\n").count()), (0, 1), "{text}");

    pin("xyce");
    let (code, text) = cli(&["netlist", "tb.shbench"]);
    assert!(
        code == 0 && text.starts_with("* tb (schemify xyce)\n"),
        "{text}"
    );
    let (code, text) = cli(&["run", "tb.shbench"]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("kit pins simulator xyce"), "{text}");

    pin("espice");
    let missing = init.replace("path = 'sh'", "path = '/nonexistent/espice'");
    std::fs::write(dir.join("init.lua"), missing).unwrap();
    let (code, text) = cli(&["run", "tb.shbench"]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("kit pins simulator espice, which is not installed"),
        "{text}"
    );

    // HSPICE: netlists, never runs (no fallback to another simulator).
    pin("hspice");
    let (code, text) = cli(&["netlist", "tb.shbench"]);
    assert!(
        code == 0 && text.starts_with("* tb (schemify hspice)\n"),
        "{text}"
    );
    let (code, text) = cli(&["run", "tb.shbench"]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("kit pins simulator hspice, which Schemify cannot start"),
        "{text}"
    );

    pin("vacask");
    let (code, text) = cli(&["netlist", "tb.shbench"]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("kit pins simulator `vacask`"), "{text}");
}

/// Xyce, LTspice and Spectre run through their `schemify.sim` rows like ngspice (run spec
/// §5): each job is netlisted in the simulator's dialect and started with its command
/// line; LTspice, which names its raw file after the deck (`netlist.raw`, no `{raw}`),
/// is read through the `*.raw` fallback. HSPICE is netlist-only.
#[test]
fn run_starts_every_simulator() {
    let dir = run_project("run-sims");
    // A resistor Spectre can spell too.
    let res = RES.to_owned() + "  format spectre \"{name} ({A} {B}) resistor r=1k\"\n";
    std::fs::write(dir.join("res.shprim"), res).unwrap();
    let ltspice = format!(
        "schemify.sim('ltspice', {{ path = 'sh', args = {{ '-c', [==[{FAKE}]==], 'fake', \
         '{{netlist}}', 'netlist.raw' }}, env = {{ WANT = 'ltspice' }} }})\n"
    );
    let init = fake_sim("xyce", ", env = { WANT = 'xyce' }")
        + &fake_sim("spectre", ", env = { WANT = 'spectre' }")
        + &ltspice;
    std::fs::write(dir.join("init.lua"), &init).unwrap();
    for sim in ["xyce", "ltspice", "spectre"] {
        let (code, text) = schemify(
            &dir,
            &["-u", "init.lua", "run", "tb.shbench", "--dialect", sim],
        );
        assert_eq!(
            (code, text.matches(" done\n").count()),
            (0, 1),
            "{sim}: {text}"
        );
    }
    let (code, text) = schemify(&dir, &["netlist", "tb.shbench", "--dialect", "hspice"]);
    assert!(
        code == 0 && text.starts_with("* tb (schemify hspice)\n"),
        "{text}"
    );
    let (code, text) = schemify(
        &dir,
        &["-u", "init.lua", "run", "tb.shbench", "--dialect", "hspice"],
    );
    assert_eq!(code, 2, "hspice is netlist-only: {text}");
}
