//! PDK manager (docs/spec/pdk.md): installs PDKs per project through `ciel`, imports their
//! xschem symbols with `schemify import`, switches the project between versions and
//! technologies by rewriting `schemify.project`, and removes what nothing references.
//!
//! - this module — the pure parts: [`RECIPES`] (spec §2, one row per PDK family), store paths
//!   (§1), the exact argv of every step (§3), the manifest rewrite of `pdk.use` (§3.1), which
//!   versions are active or locked, and the prune plan;
//! - [`panel`] — the docked "PDKs" panel (§4) as UI records over a [`Model`];
//! - on wasm32, the guest glue: commands → I/O chains (`fs_read`, `fs_write`, `fs_remove`,
//!   `spawn:ciel`, `spawn:schemify`) → model → panel.
//!
//! Six questions: in, command args and I/O completions (manifest, lockfile, the install
//! index, `STATE` files, child-process output); out, I/O requests and UI/command sets. Rows:
//! ≤ [`INSTALLS_MAX`] installs, ≤ [`REMOTE_MAX`] remote versions shown. Lifetime: the plugin
//! session; everything durable is on disk under `.schemify/pdks/`.

pub mod panel;

use schemify_abi::text;

/// One PDK family (pdk spec §2). Adding a family is adding a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recipe {
    /// Family name: the store directory and the name commands take.
    pub family: &'static str,
    /// ciel's `--pdk-family` (`ihp-sg13` for the `ihp-sg13g2` family: ciel names the family,
    /// not the variant).
    pub ciel: &'static str,
    /// The variant directory under ciel's PDK root (`sky130A`).
    pub variant: &'static str,
    /// ciel `-l` libraries; empty = ciel's default set.
    pub libs: &'static [&'static str],
    /// The models file under the variant (`set PDK_MODELS`).
    pub models: &'static str,
    /// Model sections in `tt ff ss sf fs` order (spec §7).
    pub corners: [&'static str; 5],
    /// The shipped `tech.map` (spec §6), written into the store at install.
    pub map: &'static str,
}

/// The ciel families of the MVP (spec §10). Section names checked against the upstream model
/// files and installed trees (spec Q1, checked with ciel 3.0.1 on 2026-10-12).
pub const RECIPES: [Recipe; 3] = [
    Recipe {
        family: "sky130",
        ciel: "sky130",
        variant: "sky130A",
        libs: &["sky130_fd_pr"],
        models: "libs.tech/combined/sky130.lib.spice",
        corners: ["tt", "ff", "ss", "sf", "fs"],
        map: include_str!("../maps/sky130.map"),
    },
    Recipe {
        family: "gf180mcu",
        ciel: "gf180mcu",
        variant: "gf180mcuD",
        libs: &["gf180mcu_fd_pr"],
        models: "libs.tech/ngspice/sm141064.ngspice",
        corners: ["typical", "ff", "ss", "sf", "fs"],
        map: include_str!("../maps/gf180mcu.map"),
    },
    Recipe {
        family: "ihp-sg13g2",
        ciel: "ihp-sg13",
        variant: "ihp-sg13g2",
        libs: &[],
        models: "libs.tech/ngspice/models/cornerMOSlv.lib",
        corners: ["mos_tt", "mos_ff", "mos_ss", "mos_sf", "mos_fs"],
        map: include_str!("../maps/ihp-sg13g2.map"),
    },
];

/// A `(family, version)` pair: one install.
pub type Fv = (String, String);

/// Most installs the index tracks.
pub const INSTALLS_MAX: usize = 256;
/// Most remote versions the panel lists (newest first, as ciel prints them).
pub const REMOTE_MAX: usize = 24;
/// The store (spec §1).
pub const STORE: &str = ".schemify/pdks";
/// The install index: one `<family> <version>` line per install started (the plugin cannot
/// list directories, so it records what it created).
pub const INDEX: &str = ".schemify/pdks/INSTALLS";

/// Returns the recipe of `family`.
pub fn recipe(family: &str) -> Option<&'static Recipe> {
    RECIPES.iter().find(|r| r.family == family)
}

/// Whether `v` may name a version: a ciel version id (letters, digits, `.`, `_`, `-`; at
/// most 64 bytes), never `latest`, never a path.
pub fn valid_version(v: &str) -> bool {
    let ok = !v.is_empty()
        && v.len() <= 64
        && v != "latest"
        && !v.starts_with(['.', '-'])
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    assert!(!ok || !v.contains('/'));
    ok
}

/// `.schemify/pdks/<family>/<version>`.
pub fn version_dir(family: &str, version: &str) -> String {
    assert!(recipe(family).is_some() && valid_version(version));
    format!("{STORE}/{family}/{version}")
}

/// `ciel ls-remote --pdk-family <family>`.
pub fn ls_remote_argv(r: &Recipe) -> Vec<String> {
    ["ciel", "ls-remote", "--pdk-family", r.ciel]
        .map(str::to_owned)
        .to_vec()
}

/// `ciel enable --pdk-family F --pdk-root <dir>/root [-l LIB]… V`: fetches the version and
/// links `<root>/<variant>` to it (spec §3 step 1).
pub fn fetch_argv(r: &Recipe, version: &str) -> Vec<String> {
    let root = format!("{}/root", version_dir(r.family, version));
    let mut argv: Vec<String> = ["ciel", "enable", "--pdk-family", r.ciel, "--pdk-root"]
        .map(str::to_owned)
        .to_vec();
    argv.push(root);
    for l in r.libs {
        argv.push("-l".to_owned());
        argv.push((*l).to_owned());
    }
    argv.push(version.to_owned());
    assert_eq!(argv.len(), 7 + 2 * r.libs.len());
    argv
}

/// `schemify import <root>/<variant>/libs.tech/xschem -o <dir>/lib --xschemrc …/xschemrc`
/// (spec §3 step 2).
pub fn import_argv(r: &Recipe, version: &str) -> Vec<String> {
    let dir = version_dir(r.family, version);
    let xschem = format!("{dir}/root/{}/libs.tech/xschem", r.variant);
    let argv = vec![
        "schemify".to_owned(),
        "import".to_owned(),
        xschem.clone(),
        "-o".to_owned(),
        format!("{dir}/lib"),
        "--xschemrc".to_owned(),
        format!("{xschem}/xschemrc"),
    ];
    assert_eq!(argv[0], "schemify");
    argv
}

/// Version ids in `ciel ls-remote` output: every 40-digit hex word, first occurrence order.
pub fn parse_versions(out: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for word in out.split(|c: char| !c.is_ascii_hexdigit()) {
        if word.len() == 40 && !v.iter().any(|x| x == word) && v.len() < REMOTE_MAX {
            v.push(word.to_ascii_lowercase());
        }
    }
    assert!(v.len() <= REMOTE_MAX);
    v
}

/// Parses the install index (`<family> <version>` per line; malformed lines are skipped).
pub fn parse_index(text: &str) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for line in text.lines().take(INSTALLS_MAX * 2) {
        let mut w = line.split_whitespace();
        if let (Some(f), Some(v), None) = (w.next(), w.next(), w.next())
            && recipe(f).is_some()
            && valid_version(v)
            && !rows.iter().any(|r| r.0 == f && r.1 == v)
            && rows.len() < INSTALLS_MAX
        {
            rows.push((f.to_owned(), v.to_owned()));
        }
    }
    rows
}

/// Writes the install index (sorted, so the file is deterministic).
pub fn write_index(rows: &[(String, String)]) -> String {
    let mut sorted = rows.to_vec();
    sorted.sort();
    sorted.dedup();
    let text: String = sorted.iter().map(|(f, v)| format!("{f} {v}\n")).collect();
    assert_eq!(text.lines().count(), sorted.len());
    text
}

/// The `(family, version)` a store path names, if it is under [`STORE`] (`…/.schemify/pdks/F/V/…`;
/// a lockfile row may hold the absolute path).
pub fn store_version(path: &str) -> Option<(String, String)> {
    let at = path.find(".schemify/pdks/")?;
    let mut seg = path[at + ".schemify/pdks/".len()..].split('/');
    let (f, v) = (seg.next()?, seg.next()?);
    (recipe(f).is_some() && valid_version(v)).then(|| (f.to_owned(), v.to_owned()))
}

/// One record of a line-record file as the manifest rewrite needs it (format spec §2): its
/// word, its unescaped fields, and its text lines as written (comments before it, the record
/// line, its attribute and verbatim lines), so a kept record is copied byte for byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// The record word.
    pub word: String,
    /// Its positional values, unescaped.
    pub fields: Vec<String>,
    /// Its lines: leading comments, the record line, indented lines.
    pub lines: Vec<String>,
}

/// Most records a manifest or lockfile may hold here.
pub const BLOCKS_MAX: usize = 1 << 12;

/// Splits a line-record file of header kind `kind` into its header lines and its records.
///
/// ponytail: a lexical split (record words and fields, quoting through `abi::text`), not the
/// full format reader: the guest stays small, and the app re-parses the manifest on reload,
/// reporting what this would have missed.
///
/// # Errors
///
/// A wrong header, a record line that is not a word with values, an indented line before
/// any record, or more than [`BLOCKS_MAX`] records.
pub fn blocks(text: &str, kind: &str) -> Result<(Vec<String>, Vec<Block>), String> {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let header_ok = first.split_whitespace().collect::<Vec<_>>() == ["schemify", "1", kind];
    if !header_ok {
        return Err(format!("not a `{kind}` file"));
    }
    let mut header = vec![first.to_owned()];
    let mut out: Vec<Block> = Vec::new();
    let mut comments: Vec<String> = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            comments.push(trimmed.to_owned());
            continue;
        }
        if line.starts_with([' ', '\t']) {
            match out.last_mut() {
                Some(b) => b.lines.push(line.to_owned()),
                None if comments.is_empty() => header.push(line.to_owned()),
                None => return Err("an indented line after a comment".to_owned()),
            }
            continue;
        }
        let (word, rest) = text::split_word(line).map_err(|_| format!("not a record: `{line}`"))?;
        let mut fields = Vec::new();
        for v in text::values(rest) {
            fields.push(
                v.map_err(|_| format!("bad value in `{line}`"))?
                    .into_owned(),
            );
        }
        if out.len() >= BLOCKS_MAX {
            return Err(format!("more than {BLOCKS_MAX} records"));
        }
        let mut lines = std::mem::take(&mut comments);
        lines.push(line.to_owned());
        out.push(Block {
            word: word.to_owned(),
            fields,
            lines,
        });
    }
    assert!(!header.is_empty());
    Ok((header, out))
}

/// Writes header lines and records in the canonical layout: one blank line after the header
/// and between consecutive records with different words.
pub fn write_blocks(header: &[String], blocks: &[Block]) -> String {
    let mut t = String::new();
    for l in header {
        t.push_str(l);
        t.push('\n');
    }
    for (i, b) in blocks.iter().enumerate() {
        if i == 0 || blocks[i - 1].word != b.word {
            t.push('\n');
        }
        for l in &b.lines {
            t.push_str(l);
            t.push('\n');
        }
    }
    assert!(t.ends_with('\n'));
    t
}

/// A new one-line record `word field…` (values quoted as needed).
pub fn record(word: &str, fields: &[&str]) -> Block {
    let mut line = word.to_owned();
    for f in fields {
        line.push(' ');
        text::push_value(&mut line, f);
    }
    Block {
        word: word.to_owned(),
        fields: fields.iter().map(|f| (*f).to_owned()).collect(),
        lines: vec![line],
    }
}

/// Versions the manifest uses (`active`: a `lib` record whose source path is in the store)
/// and the lockfile pins (`locked`), each sorted and deduplicated.
///
/// # Errors
///
/// A file that does not split into records ([`blocks`]), as text.
pub fn references(manifest: &[u8], lock: Option<&[u8]>) -> Result<(Vec<Fv>, Vec<Fv>), String> {
    let utf8 = |b: &[u8], what: &str| {
        String::from_utf8(b.to_vec()).map_err(|_| format!("{what}: not UTF-8"))
    };
    let (_, m) = blocks(&utf8(manifest, "schemify.project")?, "project")
        .map_err(|e| format!("schemify.project: {e}"))?;
    let mut active: Vec<(String, String)> = m
        .iter()
        .filter(|b| b.word == "lib")
        .filter_map(|b| b.fields.get(2).and_then(|p| store_version(p)))
        .collect();
    let mut locked = Vec::new();
    if let Some(lock) = lock {
        let (_, l) = blocks(&utf8(lock, "schemify.lock")?, "lock")
            .map_err(|e| format!("schemify.lock: {e}"))?;
        locked = l
            .iter()
            .filter(|b| b.word == "locked")
            .filter_map(|b| b.fields.get(3).and_then(|p| store_version(p)))
            .collect();
    }
    for v in [&mut active, &mut locked] {
        v.sort();
        v.dedup();
    }
    Ok((active, locked))
}

/// Why `pdk.remove F V` is refused, or `None` when it may proceed (spec §3).
pub fn remove_refusal(
    fv: &(String, String),
    active: &[(String, String)],
    locked: &[(String, String)],
) -> Option<&'static str> {
    if active.contains(fv) {
        Some("active in schemify.project: use another version first")
    } else if locked.contains(fv) {
        Some("pinned by schemify.lock")
    } else {
        None
    }
}

/// Rows of `installs` that `pdk.prune` removes: every install neither active nor locked
/// (complete or not). An active or locked install is never removed, even when incomplete.
pub fn prune_plan(
    installs: &[(String, String)],
    active: &[(String, String)],
    locked: &[(String, String)],
) -> Vec<usize> {
    let plan: Vec<usize> = (0..installs.len())
        .filter(|&i| remove_refusal(&installs[i], active, locked).is_none())
        .collect();
    assert!(plan.iter().all(|&i| !active.contains(&installs[i])));
    plan
}

/// The manifest after `pdk.use F V` (spec §3.1), canonical. Drops every `lib` whose source is
/// in the store, `lib tech`, and the `set` entries `pdk.use` owns (`PDK_ROOT`, `PDK`,
/// `PDK_MODELS`, and any whose value names `PDK_ROOT` or the store); then appends the import's
/// libraries (paths prefixed with `<dir>/lib/`), `lib tech map <dir>/tech.map`, `set PDK_ROOT`,
/// `set PDK`, `set PDK_MODELS` and the import's `set` entries. Everything else, comments
/// included, is kept in order and byte for byte.
///
/// # Errors
///
/// A file that does not split into records or is not a manifest, as text.
pub fn use_manifest(
    manifest: &[u8],
    import: &[u8],
    r: &Recipe,
    version: &str,
) -> Result<String, String> {
    let dir = version_dir(r.family, version);
    let utf8 = |b: &[u8], what: &str| {
        String::from_utf8(b.to_vec()).map_err(|_| format!("{what}: not UTF-8"))
    };
    let (header, old) = blocks(&utf8(manifest, "schemify.project")?, "project")
        .map_err(|e| format!("schemify.project: {e}"))?;
    let (_, imp) = blocks(&utf8(import, "import manifest")?, "project")
        .map_err(|e| format!("{dir}/lib/schemify.project: {e}"))?;
    fn field(b: &Block, i: usize) -> &str {
        b.fields.get(i).map_or("", String::as_str)
    }
    let ours = |b: &Block| match b.word.as_str() {
        "lib" => field(b, 0) == "tech" || field(b, 2).starts_with(".schemify/pdks/"),
        "set" => {
            matches!(field(b, 0), "PDK_ROOT" | "PDK" | "PDK_MODELS")
                || field(b, 1).contains("PDK_ROOT")
                || field(b, 1).contains(".schemify/pdks/")
        }
        _ => false,
    };
    let mut out: Vec<Block> = old.iter().filter(|b| !ours(b)).cloned().collect();
    let kept = out.len();
    for b in imp
        .iter()
        .filter(|b| b.word == "lib" && field(b, 1) == "path")
    {
        out.push(record(
            "lib",
            &[field(b, 0), "path", &format!("{dir}/lib/{}", field(b, 2))],
        ));
    }
    out.push(record("lib", &["tech", "map", &format!("{dir}/tech.map")]));
    out.push(record("set", &["PDK_ROOT", &format!("{dir}/root")]));
    out.push(record("set", &["PDK", r.variant]));
    out.push(record(
        "set",
        &[
            "PDK_MODELS",
            &format!("${{PDK_ROOT}}/{}/{}", r.variant, r.models),
        ],
    ));
    for b in imp
        .iter()
        .filter(|b| b.word == "set" && !matches!(field(b, 0), "PDK_ROOT" | "PDK" | "PDK_MODELS"))
    {
        out.push(record("set", &[field(b, 0), field(b, 1)]));
    }
    assert!(out.len() > kept);
    Ok(write_blocks(&header, &out))
}

/// What the panel and the commands know: installs with their state, references, the
/// remote listing, the running job.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Model {
    /// The panel is shown.
    pub open: bool,
    /// Installs from the index.
    pub installs: Vec<(String, String)>,
    /// Per install: `Some(true)` when `STATE` reads `ok`, `Some(false)` incomplete, `None`
    /// not read yet.
    pub complete: Vec<Option<bool>>,
    /// Versions the manifest uses.
    pub active: Vec<(String, String)>,
    /// Versions the lockfile pins.
    pub locked: Vec<(String, String)>,
    /// The family the "Available" section lists.
    pub family: String,
    /// Its remote versions (`ls-remote`).
    pub remote: Vec<String>,
    /// The running install, if any.
    pub job: Option<Job>,
    /// The prune waiting for `pdk.prune confirm`: the installs it removes.
    pub prune: Option<Vec<(String, String)>>,
    /// The last outcome, for the panel's status row.
    pub message: String,
}

/// One install in progress (or the last one, when `failed`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// Family.
    pub family: String,
    /// Version.
    pub version: String,
    /// The step running: `fetch`, `import`, `map`, `state`.
    pub step: &'static str,
    /// The last lines of the previous step's output (at most [`TAIL_LINES`]).
    pub tail: Vec<String>,
}

/// Output lines kept per step.
pub const TAIL_LINES: usize = 6;

/// The last [`TAIL_LINES`] non-empty lines of child output.
pub fn tail(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let from = lines.len().saturating_sub(TAIL_LINES);
    let out: Vec<String> = lines[from..]
        .iter()
        .map(|l| l.chars().take(160).collect())
        .collect();
    assert!(out.len() <= TAIL_LINES);
    out
}

#[cfg(target_arch = "wasm32")]
mod guest_glue;

#[cfg(test)]
mod tests {
    use super::*;

    fn fv(f: &str, v: &str) -> (String, String) {
        (f.to_owned(), v.to_owned())
    }

    #[test]
    fn argv_is_exactly_the_spec() {
        let r = recipe("sky130").unwrap();
        assert_eq!(
            fetch_argv(r, "abc123").join(" "),
            "ciel enable --pdk-family sky130 --pdk-root .schemify/pdks/sky130/abc123/root -l sky130_fd_pr abc123"
        );
        assert_eq!(
            import_argv(r, "abc123").join(" "),
            "schemify import .schemify/pdks/sky130/abc123/root/sky130A/libs.tech/xschem -o .schemify/pdks/sky130/abc123/lib --xschemrc .schemify/pdks/sky130/abc123/root/sky130A/libs.tech/xschem/xschemrc"
        );
        let ihp = recipe("ihp-sg13g2").unwrap();
        assert_eq!(
            fetch_argv(ihp, "v1").join(" "),
            "ciel enable --pdk-family ihp-sg13 --pdk-root .schemify/pdks/ihp-sg13g2/v1/root v1"
        );
        assert_eq!(
            ls_remote_argv(ihp).join(" "),
            "ciel ls-remote --pdk-family ihp-sg13"
        );
        assert!(recipe("asap7").is_none());
    }

    #[test]
    fn versions_and_paths_are_checked() {
        for ok in [
            "0fe599b2afb6708d281543108caf8310912f54af",
            "v1.2",
            "2024_01-x",
        ] {
            assert!(valid_version(ok), "{ok}");
        }
        for bad in [
            "",
            "latest",
            "..",
            "../x",
            "a/b",
            ".hidden",
            "-l",
            "a b",
            &"x".repeat(65),
        ] {
            assert!(!valid_version(bad), "{bad}");
        }
        let out = "Pre-built sky130 PDK versions\n├── 0FE599B2AFB6708D281543108CAF8310912F54AF (2023.10.05)\n└── 12df12e2e74145e31c5a13de02f9a1e176b56e67 (2023.09.20)\n0fe599b2afb6708d281543108caf8310912f54af again\nshort 1234abc\n";
        assert_eq!(
            parse_versions(out),
            [
                "0fe599b2afb6708d281543108caf8310912f54af",
                "12df12e2e74145e31c5a13de02f9a1e176b56e67"
            ]
        );
        assert_eq!(
            store_version("/home/u/p/.schemify/pdks/sky130/v1/lib/sky130_fd_pr"),
            Some(fv("sky130", "v1"))
        );
        assert_eq!(store_version(".schemify/pdks/nope/v1"), None);
        assert_eq!(store_version("lib/x"), None);
        let idx =
            parse_index("sky130 b\nsky130 a\nbogus x\nsky130 ../x\ngf180mcu c extra\nsky130 a\n");
        assert_eq!(idx, [fv("sky130", "b"), fv("sky130", "a")]);
        assert_eq!(write_index(&idx), "sky130 a\nsky130 b\n");
    }

    const MANIFEST: &str = "schemify 1 project\n\n# my own library\nlib mine path lib\nlib sky130_fd_pr path .schemify/pdks/sky130/old/lib/sky130_fd_pr\nlib tech map .schemify/pdks/sky130/old/tech.map\n\nplugin pdk-manager path plugins/pdk-manager\n  grant write_project\n\nset PDK_ROOT .schemify/pdks/sky130/old/root\nset SKYWATER_MODELS ${PDK_ROOT}/${PDK}/libs.tech/combined\nset MINE 1\n";
    const IMPORT: &str = "schemify 1 project\nlib gf180mcu_fd_pr path gf180mcu_fd_pr\n\nset GF_MODELS ${PDK_ROOT}/${PDK}/libs.tech/ngspice\n";

    #[test]
    fn use_rewrites_only_what_it_owns() {
        let r = recipe("gf180mcu").unwrap();
        let text = use_manifest(MANIFEST.as_bytes(), IMPORT.as_bytes(), r, "v2").unwrap();
        assert_eq!(
            text,
            "schemify 1 project\n\n# my own library\nlib mine path lib\n\nplugin pdk-manager path plugins/pdk-manager\n  grant write_project\n\nset MINE 1\n\nlib gf180mcu_fd_pr path .schemify/pdks/gf180mcu/v2/lib/gf180mcu_fd_pr\nlib tech map .schemify/pdks/gf180mcu/v2/tech.map\n\nset PDK_ROOT .schemify/pdks/gf180mcu/v2/root\nset PDK gf180mcuD\nset PDK_MODELS ${PDK_ROOT}/gf180mcuD/libs.tech/ngspice/sm141064.ngspice\nset GF_MODELS ${PDK_ROOT}/${PDK}/libs.tech/ngspice\n"
        );
        // The independent oracle: the format writer reproduces it (canonical, parses).
        let d = schemify_format::parse(text.as_bytes()).expect("parses");
        assert_eq!(schemify_format::write(&d), text);
        // Idempotent: using the same version again changes nothing.
        let again = use_manifest(text.as_bytes(), IMPORT.as_bytes(), r, "v2").unwrap();
        assert_eq!(again, text);
        let (active, _) = references(text.as_bytes(), None).unwrap();
        assert_eq!(active, [fv("gf180mcu", "v2")]);
        assert!(use_manifest(b"schemify 1 lock\n", IMPORT.as_bytes(), r, "v2").is_err());
        assert!(use_manifest(b"not a file", IMPORT.as_bytes(), r, "v2").is_err());
    }

    #[test]
    fn references_and_prune_plan() {
        let lock = "schemify 1 lock\nlocked lib sky130_fd_pr path /p/.schemify/pdks/sky130/locked/lib/sky130_fd_pr 00\nlocked lib mine path /p/lib 11\n";
        let (active, locked) = references(MANIFEST.as_bytes(), Some(lock.as_bytes())).unwrap();
        assert_eq!(active, [fv("sky130", "old")]);
        assert_eq!(locked, [fv("sky130", "locked")]);
        let installs = vec![
            fv("sky130", "old"),
            fv("sky130", "locked"),
            fv("sky130", "spare"),
            fv("gf180mcu", "broken"),
        ];
        assert_eq!(prune_plan(&installs, &active, &locked), [2, 3]);
        assert!(
            remove_refusal(&installs[0], &active, &locked)
                .unwrap()
                .contains("active")
        );
        assert!(
            remove_refusal(&installs[1], &active, &locked)
                .unwrap()
                .contains("schemify.lock")
        );
        assert_eq!(remove_refusal(&installs[2], &active, &locked), None);
    }

    #[test]
    fn tails_keep_the_last_lines() {
        let out: String = (0..20).map(|i| format!("line {i}\n\n")).collect();
        assert_eq!(
            tail(out.as_bytes()),
            [
                "line 14", "line 15", "line 16", "line 17", "line 18", "line 19"
            ]
        );
        assert!(tail(b"").is_empty());
    }

    #[test]
    fn shipped_maps_are_tech_maps() {
        for r in RECIPES {
            let d = schemify_format::parse(r.map.as_bytes()).expect(r.family);
            assert_eq!(d.kind(), schemify_format::FileKind::Techmap);
        }
    }
}
