//! `schemify annotate` end to end (docs/guide/cli.md `annotate`, netlist spec §2): an
//! extracted netlist becomes a `pex` impl of its cell, its ports checked against the cell's
//! pins by name, and every bench that reaches the cell gets `config post`.
//!
//! Oracles: the plain ESPice deck on the same extracted file (no Schemify code between the
//! file and the simulator) for the numbers; the unswapped netlist for the swapped one; the
//! extracted file itself for the netlist text; the files as they were for every refusal;
//! `schemify rpc`'s `edit` for `--dry-run`'s op text; reviewed goldens
//! (`tests/fixtures/annotate/golden/`, re-blessed with `SCHEMIFY_BLESS=1`) for the files
//! annotate writes. The simulating tests need `espice` on PATH and the sky130 models
//! (`SCHEMIFY_SKY130_LIB`, default `~/.volare/sky130A/libs.tech/ngspice/sky130.lib.spice`);
//! they skip without them unless `SCHEMIFY_REQUIRE_ESPICE` is set.

mod common;

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::{core, scratch};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/annotate");

/// The bench's lines of the plain deck: the same sources, loads and analysis as
/// `tb.shbench`'s `code` block, and the outputs as ESPice's own `.meas`.
const DECK: &str = "VDD vdd 0 1.8\nVCM cm 0 0.9\nVinp inp cm dc 0 ac 0.5\nVinn inn cm dc 0 ac -0.5\n\
RL1 vdd outp 10k\nRL2 vdd outn 10k\nItail tail 0 100u\nEdiff od 0 outp outn 1\n.ac dec 10 1k 10g\n\
.meas ac gdiff find vdb(od) at=1k\n.meas ac gse find vdb(outp) at=1k\n";

// ---- private ----

/// Runs the core in `dir` with the user directories inside it (no developer `init.lua`).
fn schemify(dir: &Path, args: &[&str]) -> (i32, String) {
    let (code, mut out, err) = schemify_io(dir, args, "");
    out.push_str(&err);
    (code, out)
}

/// [`schemify`] with `stdin`, stdout and stderr apart.
fn schemify_io(dir: &Path, args: &[&str], stdin: &str) -> (i32, String, String) {
    let mut child = core(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary runs");
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(stdin.as_bytes())
        .expect("the child reads stdin");
    let out = child.wait_with_output().expect("binary runs");
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (
        out.status.code().expect("not killed"),
        text(&out.stdout),
        text(&out.stderr),
    )
}

/// Copies directory `from` into `to`, recursively.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("the scratch dir is writable");
    for e in fs::read_dir(from).expect("the fixture exists").flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_tree(&p, &to.join(e.file_name()));
        } else {
            fs::copy(&p, to.join(e.file_name())).expect("the scratch dir is writable");
        }
    }
}

/// A fresh copy of the fixture project (goldens left out).
fn project(name: &str) -> PathBuf {
    let dir = scratch(name);
    copy_tree(Path::new(FIXTURE), &dir);
    fs::remove_dir_all(dir.join("golden")).expect("the copy has the goldens");
    dir
}

/// The text of `file` in `dir`.
fn read(dir: &Path, file: &str) -> String {
    fs::read_to_string(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"))
}

/// Every file under `dir` with its bytes (the state a refusal must leave alone).
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in fs::read_dir(&d).expect("a listed directory").flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else {
                out.insert(p.clone(), fs::read(&p).expect("a listed file"));
            }
        }
    }
    out
}

/// Asserts `text` equals golden `name` (written first under `SCHEMIFY_BLESS`).
fn golden(name: &str, text: &str) {
    let path = Path::new(FIXTURE).join("golden").join(name);
    if env::var_os("SCHEMIFY_BLESS").is_some() {
        fs::write(&path, text).expect("the fixture is writable");
    }
    let want = fs::read_to_string(&path).unwrap_or_else(|_| panic!("{name}: bless it"));
    assert_eq!(text, want, "{name}");
}

/// A refusal names what is wrong and writes nothing.
fn refused(dir: &Path, args: &[&str], says: &[&str]) {
    let before = snapshot(dir);
    let (code, text) = schemify(dir, args);
    assert_eq!(code, 1, "{args:?}: {text}");
    for s in says {
        assert!(text.contains(s), "{args:?} should say `{s}`: {text}");
    }
    assert_eq!(snapshot(dir), before, "{args:?} wrote a file");
}

/// [`snapshot`] keyed by path relative to `dir`, so two projects compare.
fn snapshot_rel(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    snapshot(dir)
        .into_iter()
        .filter(|(p, _)| !p.starts_with(dir.join(".xdg")))
        .map(|(p, b)| (p.strip_prefix(dir).expect("under dir").to_path_buf(), b))
        .collect()
}

/// A JSON string literal.
fn json(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The sky130 model library, when `espice` runs from PATH and the library exists; skips
/// (`None`) otherwise, or fails under `SCHEMIFY_REQUIRE_ESPICE`.
fn sky130() -> Option<PathBuf> {
    let lib = env::var_os("SCHEMIFY_SKY130_LIB").map_or_else(
        || {
            let home = env::var_os("HOME").unwrap_or_default();
            Path::new(&home).join(".volare/sky130A/libs.tech/ngspice/sky130.lib.spice")
        },
        PathBuf::from,
    );
    let espice = Command::new("espice").arg("--version").output().is_ok();
    if espice && lib.is_file() {
        return Some(lib);
    }
    assert!(
        env::var_os("SCHEMIFY_REQUIRE_ESPICE").is_none(),
        "SCHEMIFY_REQUIRE_ESPICE: espice on PATH ({espice}) and {} ({})",
        lib.display(),
        lib.is_file()
    );
    eprintln!("espice or the sky130 models missing: skipping");
    None
}

/// The fixture project with `PDK_MODELS` naming the real sky130 library.
fn sky130_project(name: &str, lib: &Path) -> PathBuf {
    let dir = project(name);
    let manifest = format!("schemify 1 project\n\nset PDK_MODELS {}\n", lib.display());
    fs::write(dir.join("schemify.project"), manifest).expect("the scratch dir is writable");
    dir
}

/// `gdiff` and `gse` of the plain ESPice deck that includes `netlist` (in `dir`) and calls
/// it with `call`, the nets in the netlist's own port order.
fn plain_deck(dir: &Path, lib: &Path, netlist: &str, call: &str) -> [f64; 2] {
    let deck = format!(
        "* plain deck: {netlist}\n.lib \"{}\" tt\n.include \"{netlist}\"\n{DECK}{call}\n.end\n",
        lib.display()
    );
    let name = format!("plain.{netlist}.sp");
    fs::write(dir.join(&name), deck).expect("the scratch dir is writable");
    let out = Command::new("espice")
        .args(["-b", "--tokenizer=ngspice", &name])
        .current_dir(dir)
        .output()
        .expect("espice runs");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meas = |m: &str| -> f64 {
        let line = text
            .lines()
            .find(|l| l.split_whitespace().next() == Some(m))
            .unwrap_or_else(|| panic!("no .meas {m}: {text}"));
        let value = line.split('=').nth(1).expect("`name = value`");
        value.trim().parse().expect("a number")
    };
    [meas("gdiff"), meas("gse")]
}

/// Whether a run's value equals a `.meas` result, which ESPice prints to 7 significant
/// digits (a relative error of at most 5e-7).
fn close(run: f64, meas: f64) -> bool {
    (run - meas).abs() <= 1e-6 * meas.abs()
}

/// `gdiff` and `gse` per config of `schemify run tb.shbench --config C…` with ESPice.
fn run_configs(dir: &Path, configs: &[&str]) -> BTreeMap<String, [f64; 2]> {
    let mut args = vec!["run", "tb.shbench", "--dialect", "espice"];
    for c in configs {
        args.extend(["--config", c]);
    }
    let (code, out, err) = schemify_io(dir, &args, "");
    assert_eq!(code, 0, "{out}{err}");
    let mut values: BTreeMap<String, [f64; 2]> = BTreeMap::new();
    let mut config = String::new();
    for line in out.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.as_slice() {
            ["config", c] => config = (*c).to_owned(),
            ["value", name, v] => {
                let k = usize::from(*name == "gse");
                values.entry(config.clone()).or_insert([f64::NAN; 2])[k] =
                    v.parse().expect("a number");
            }
            _ => {}
        }
    }
    assert_eq!(values.len(), configs.len(), "{out}");
    values
}

/// Gate: a netlist missing a pin is refused, naming it (and the port the cell lacks).
#[test]
fn a_netlist_missing_a_pin_is_refused_by_name() {
    let dir = project("missing");
    refused(
        &dir,
        &["annotate", "dp.shcomp", "dp.missing.spice"],
        &["missing tail", "extra VSUBS"],
    );
    // Negative space: no `.subckt` of the cell's name, a bench as the cell, a missing file.
    refused(
        &dir,
        &["annotate", "dp.shcomp", "dp_layout.spice"],
        &["no .subckt dp", "dp_layout"],
    );
    let (code, text) = schemify(&dir, &["annotate", "tb.shbench", "dp.magic.spice"]);
    assert_eq!(code, 2, "a bench has no pins: {text}");
    let (code, text) = schemify(&dir, &["annotate", "dp.shcomp", "nowhere.spice"]);
    assert_eq!(code, 2, "{text}");
    assert_eq!(schemify(&dir, &["annotate", "dp.shcomp"]).0, 2);
    let (code, text) = schemify(
        &dir,
        &["annotate", "dp.shcomp", "dp.magic.spice", "--impl", "a b"],
    );
    assert_eq!(code, 2, "an impl name is an identifier: {text}");
    // An impl of another language is never replaced.
    refused(
        &dir,
        &["annotate", "dp.shcomp", "dp.magic.spice", "--impl", "sch"],
        &["sch", "schematic"],
    );
}

/// Golden: the Magic netlist (ports in Magic's order, its `.subckt` named after the cell)
/// becomes `impl pex` with a wrapper in the cell's pin order (named `dp__pex`, since the file
/// has its own `dp`, and called by a `format spice` line), the header's `default` keeps
/// `sch`, and both benches that reach dp get `config post`, the one one level down too; the
/// bench that does not reach dp is left alone.
#[test]
fn annotate_writes_the_impl_and_the_configs() {
    let dir = project("magic");
    let untouched = read(&dir, "nfet_tb.shbench");
    let (code, out, err) = schemify_io(&dir, &["annotate", "dp.shcomp", "dp.magic.spice"], "");
    assert_eq!(code, 0, "{out}{err}");
    assert!(!err.contains("warning"), "{err}");
    golden("dp.magic.shcomp", &read(&dir, "dp.shcomp"));
    golden("tb.shbench", &read(&dir, "tb.shbench"));
    golden("top_tb.shbench", &read(&dir, "top_tb.shbench"));
    golden("annotate.magic.out", &out);
    assert_eq!(read(&dir, "nfet_tb.shbench"), untouched);
    // Every file annotate wrote is canonical.
    let (code, text) = schemify(
        &dir,
        &[
            "fmt",
            "--check",
            "dp.shcomp",
            "tb.shbench",
            "top_tb.shbench",
        ],
    );
    assert_eq!(code, 0, "{text}");
}

/// Golden: the GPurify netlist is in the cell's pin order and named after the cell, so it
/// is used as is: no wrapper, no `format` line.
#[test]
fn a_netlist_in_pin_order_is_used_as_is() {
    let dir = project("gpurify");
    let (code, text) = schemify(&dir, &["annotate", "dp.shcomp", "dp.gpurify.spice"]);
    assert_eq!(code, 0, "{text}");
    golden("dp.gpurify.shcomp", &read(&dir, "dp.shcomp"));
    golden("tb.shbench", &read(&dir, "tb.shbench"));
}

/// `--subckt` picks a differently named `.subckt`. With no `.subckt dp` in the file the
/// wrapper takes the cell's name, which the default template calls (no `format` line).
#[test]
fn subckt_names_another_definition() {
    let dir = project("layout");
    let args = [
        "annotate",
        "dp.shcomp",
        "dp_layout.spice",
        "--subckt",
        "dp_layout",
    ];
    let (code, text) = schemify(&dir, &args);
    assert_eq!(code, 0, "{text}");
    golden("dp.layout.shcomp", &read(&dir, "dp.shcomp"));
}

/// The post-layout netlist carries the extracted file byte for byte, through the
/// netlister's own `${NAME}` expansion (Magic's `$` comments, and a `${HOME}` and `$$` the
/// expansion would read); every other config's netlist is the one before the annotation.
#[test]
fn the_netlist_carries_the_extracted_text_verbatim() {
    let dir = project("verbatim");
    let magic = read(&dir, "dp.magic.spice");
    let with_dollars = magic.replacen('\n', "\n* paths: ${HOME} $$ $x\n", 1);
    fs::write(dir.join("dp.magic.spice"), &with_dollars).unwrap();
    let netlist = |config: &[&str]| {
        let mut args = vec!["netlist", "tb.shbench", "--dialect", "espice"];
        args.extend_from_slice(config);
        let (code, text, err) = schemify_io(&dir, &args, "");
        assert_eq!(code, 0, "{config:?}: {err}");
        text
    };
    let current = netlist(&["--config", "current"]);
    let default = netlist(&[]);
    let (code, text) = schemify(&dir, &["annotate", "dp.shcomp", "dp.magic.spice"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(netlist(&["--config", "current"]), current);
    assert_eq!(netlist(&[]), default);
    let post = netlist(&["--config", "post"]);
    let extracted = read(&dir, "dp.magic.spice");
    assert!(post.contains(&extracted), "{post}");
    assert!(
        post.contains("\nXX1 inp inn outp outn tail 0 dp__pex\n")
            && post.contains(".subckt dp__pex inp inn outp outn tail vss\nX1 outn outp inp inn tail vss dp\n.ends dp__pex\n"),
        "{post}"
    );
}

/// Re-running after a new extraction replaces the impl and keeps the configs: annotating
/// the Magic netlist and then the GPurify one leaves the files one GPurify run writes.
#[test]
fn rerun_replaces_the_impl_and_keeps_the_configs() {
    let twice = project("rerun-twice");
    let once = project("rerun-once");
    for file in ["dp.magic.spice", "dp.gpurify.spice"] {
        let (code, text) = schemify(&twice, &["annotate", "dp.shcomp", file]);
        assert_eq!(code, 0, "{file}: {text}");
    }
    let (code, text) = schemify(&once, &["annotate", "dp.shcomp", "dp.gpurify.spice"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(snapshot_rel(&twice), snapshot_rel(&once));
    // A config that already selects the cell is left as it is, even naming another impl.
    let tb = read(&once, "tb.shbench").replace("cell ./dp pex", "cell ./dp sch");
    fs::write(once.join("tb.shbench"), &tb).unwrap();
    let (code, text) = schemify(&once, &["annotate", "dp.shcomp", "dp.magic.spice"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(read(&once, "tb.shbench"), tb);
    assert!(text.contains("kept"), "{text}");
}

/// `--dry-run` writes nothing and prints, per file, the op text (edits spec §1) annotate
/// applies: `schemify rpc`'s `edit` applying it gives the files a real run writes.
#[test]
fn dry_run_prints_the_ops_it_would_apply() {
    let dry = project("dry");
    let real = project("dry-real");
    let before = snapshot(&dry);
    let args = ["annotate", "dp.shcomp", "dp.magic.spice", "--dry-run"];
    let (code, out, err) = schemify_io(&dry, &args, "");
    assert_eq!(code, 0, "{err}");
    assert_eq!(snapshot(&dry), before, "--dry-run wrote a file");
    golden("dry-run.magic.out", &out);
    // `== <path>` heads each file's op text (no op text line starts with `=`).
    let mut files: Vec<(&str, String)> = Vec::new();
    for line in out.lines() {
        match line.strip_prefix("== ") {
            Some(file) => files.push((file, String::new())),
            None => {
                let ops = &mut files.last_mut().expect("a file heads the ops").1;
                ops.push_str(line);
                ops.push('\n');
            }
        }
    }
    assert_eq!(files.len(), 3, "{out}");
    let mut requests = String::new();
    for (k, (file, ops)) in files.iter().enumerate() {
        requests += &format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{k},\"method\":\"edit\",\"params\":{{\"file\":{},\"ops\":{},\"label\":\"annotate\"}}}}\n",
            json(file),
            json(ops)
        );
    }
    let (code, answers, err) = schemify_io(&dry, &["rpc"], &requests);
    assert_eq!(code, 0, "{err}");
    assert_eq!(answers.matches("\"changed\":true").count(), 3, "{answers}");
    let (code, text) = schemify(&real, &["annotate", "dp.shcomp", "dp.magic.spice"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(snapshot_rel(&dry), snapshot_rel(&real));
}

/// A device the corner's model files do not define is reported at annotate time, which still
/// writes; the devices they define are not reported.
#[test]
fn devices_the_kit_lacks_are_reported() {
    let dir = project("kit");
    let text = read(&dir, "dp.magic.spice").replacen(
        "VSS sky130_fd_pr__nfet_01v8",
        "VSS sky130_fd_pr__pfet_01v8_hvt",
        1,
    );
    fs::write(dir.join("dp.hvt.spice"), text).unwrap();
    let (code, _, err) = schemify_io(&dir, &["annotate", "dp.shcomp", "dp.hvt.spice"], "");
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("sky130_fd_pr__pfet_01v8_hvt") && err.contains("corner tt"),
        "{err}"
    );
    assert!(!err.contains("sky130_fd_pr__nfet_01v8"), "{err}");
    assert!(read(&dir, "dp.shcomp").contains("impl pex spice"));
}

/// A cell with a bus pin and an inherited pin, its extracted ports in another order: the
/// wrapper's ports are the cell's port bits in the order the netlister connects them
/// (ordinary pins' bits as written, then inherited pins), and it calls the extracted
/// `.subckt` with each port on the node of its own name (the netlist text as the oracle).
#[test]
fn bus_and_inherited_pins_bind_by_bit_name() {
    let dir = project("bus");
    fs::write(
        dir.join("reg.shprim"),
        "schemify 1 shprim\n  prefix X\n\npin d[1:0] in -20 0\npin q out 20 0\n\
         pin vdd inout 0 -20\n  inherit vdd!\n\nimpl beh spice\n  \
         format spice \"X{name} {pins} reg_beh\"\n\nrect -10 -10 10 10\n",
    )
    .unwrap();
    fs::write(
        dir.join("reg.pex.spice"),
        ".subckt reg VDD q d[0] d[1]\nR1 d[0] q 1k\nR2 d[1] q 1k\nR3 VDD q 1k\n.ends reg\n",
    )
    .unwrap();
    fs::write(
        dir.join("reg_tb.shbench"),
        "schemify 1 shbench\n\ninst 0x0001 ./reg\n  at 0 0 R0\n  name X1\n\n\
         label 0n0001 -20 0 R0 d[1:0]\nlabel 0n0002 20 0 R0 q\n",
    )
    .unwrap();
    let (code, text) = schemify(&dir, &["annotate", "reg.shprim", "reg.pex.spice"]);
    assert_eq!(code, 0, "{text}");
    let (code, post) = schemify(
        &dir,
        &[
            "netlist",
            "reg_tb.shbench",
            "--dialect",
            "ngspice",
            "--config",
            "post",
        ],
    );
    assert_eq!(code, 0, "{post}");
    let words = |prefix: &str| -> Vec<String> {
        let line = post
            .lines()
            .find(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no `{prefix}` line: {post}"));
        line.split_whitespace().map(str::to_owned).collect()
    };
    // The bench row: its nets in the netlister's order, then the wrapper.
    let row = words("XX1 ");
    let wrapper = words(".subckt reg__pex ");
    assert_eq!(
        row[1..row.len() - 1],
        ["d[1]", "d[0]", "q", "vdd!"],
        "{post}"
    );
    assert_eq!(row.last().unwrap(), "reg__pex");
    assert_eq!(wrapper[2..], ["d[1]", "d[0]", "q", "vdd"], "{post}");
    // Inside: the extracted ports in their own order, each on the node of its name.
    let call = words("X1 ");
    assert_eq!(call[1..], ["vdd", "q", "d[0]", "d[1]", "reg"], "{post}");
}

/// Gate: the sky130 pair extracted by Magic (ports in Magic's order: a wrapper) and by
/// GPurify (in pin order: used as is), annotated and run with `--config post`, reproduces
/// the plain ESPice deck on the same netlist; post-layout is not the schematic (negative
/// space: the parasitics move the gain).
#[test]
fn post_layout_runs_reproduce_the_plain_espice_deck() {
    let Some(lib) = sky130() else { return };
    for (netlist, call) in [
        ("dp.magic.spice", "X1 outn outp inp inn tail 0 dp"),
        ("dp.gpurify.spice", "X1 inp inn outp outn tail 0 dp"),
    ] {
        let dir = sky130_project(netlist, &lib);
        let (code, out, err) = schemify_io(&dir, &["annotate", "dp.shcomp", netlist], "");
        assert_eq!(code, 0, "{out}{err}");
        assert!(
            !err.contains("warning"),
            "{netlist}: the kit defines its devices: {err}"
        );
        let runs = run_configs(&dir, &["current", "post"]);
        let plain = plain_deck(&dir, &lib, netlist, call);
        let (post, pre) = (runs["post"], runs["current"]);
        eprintln!("{netlist}: plain {plain:?}, post {post:?}, schematic {pre:?}");
        for k in 0..2 {
            assert!(close(post[k], plain[k]), "{netlist}: {post:?} vs {plain:?}");
            assert!(
                (post[k] - pre[k]).abs() > 1e-4,
                "{netlist}: post {post:?} = pre {pre:?}"
            );
        }
    }
}

/// Gate: a netlist with two ports swapped in its `.subckt` line gets a wrapper, and the bench
/// result equals the unswapped netlist's (and the plain deck's on the swapped file); used in
/// the cell's order instead, the swapped file gives another result (the swap matters).
#[test]
fn swapped_ports_give_the_unswapped_result() {
    let Some(lib) = sky130() else { return };
    let mut post = Vec::new();
    for netlist in ["dp.gpurify.spice", "dp.swapped.spice"] {
        let dir = sky130_project(netlist, &lib);
        let (code, text) = schemify(&dir, &["annotate", "dp.shcomp", netlist]);
        assert_eq!(code, 0, "{text}");
        assert_eq!(
            read(&dir, "dp.shcomp").contains(".subckt dp__pex"),
            netlist == "dp.swapped.spice",
            "only the swapped file gets a wrapper"
        );
        post.push(run_configs(&dir, &["post"])["post"]);
    }
    let dir = sky130_project("swapped-plain", &lib);
    let plain = plain_deck(
        &dir,
        &lib,
        "dp.swapped.spice",
        "X1 inp outn outp inn tail 0 dp",
    );
    let positional = plain_deck(
        &dir,
        &lib,
        "dp.swapped.spice",
        "X1 inp inn outp outn tail 0 dp",
    );
    eprintln!(
        "unswapped {:?}, swapped {:?}, plain {plain:?}, positional {positional:?}",
        post[0], post[1]
    );
    for k in 0..2 {
        assert!((post[1][k] - post[0][k]).abs() < 1e-9, "{post:?}");
        assert!(close(post[1][k], plain[k]), "{post:?} vs {plain:?}");
    }
    assert!(
        (positional[0] - plain[0]).abs() > 1.0,
        "{positional:?} vs {plain:?}"
    );
}
