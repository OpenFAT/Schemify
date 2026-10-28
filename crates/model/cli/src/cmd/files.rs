//! The file commands: design files on disk → canonical files, merged files, record diffs, and
//! git's merge-driver setup (format spec §8; D27 exit codes). No project is loaded: each
//! command reads only the files it names.
//!
//! Contents: `fn fmt(&[String]) -> Exit`, `fn merge_driver(&[String]) -> Exit`,
//! `fn diff(&[String]) -> Exit`, `fn git_setup(&[String]) -> Exit`.

use std::path::Path;

use schemify_cli::platform::{self, write_atomic};
use schemify_format as format;

use crate::{DESIGN_ERROR, Exit, OK, USAGE_ERROR, usage};

/// `fmt [--check] <file>...`: parse → canonical write, in place unless `--check`. A file that
/// does not parse is reported and skipped; a read or write failure stops at once.
pub fn fmt(args: &[String]) -> Exit {
    let check = args.iter().any(|a| a == "--check");
    let files: Vec<&String> = args.iter().filter(|a| *a != "--check").collect();
    if files.is_empty() {
        return Err(usage());
    }
    let mut worst = OK;
    for path in files {
        let bytes = read(path)?;
        let Ok(doc) = parse(path, &bytes) else {
            worst = worst.max(DESIGN_ERROR);
            continue;
        };
        let canonical = format::write(&doc);
        if canonical.as_bytes() == bytes.as_slice() {
            continue;
        }
        if check {
            eprintln!("{path}: not in canonical form");
            worst = worst.max(DESIGN_ERROR);
        } else {
            write(path, canonical.as_bytes())?;
        }
    }
    assert!(worst <= DESIGN_ERROR);
    Ok(worst)
}

/// `merge-driver O A B [P]`: semantic 3-way merge (format spec §8); the result replaces A,
/// as git expects. Exit 1 when conflict blocks remain, or when a side does not parse (A is
/// then left untouched, so git reports the conflict and the user resolves it by hand).
pub fn merge_driver(args: &[String]) -> Exit {
    let [o, a, b, ..] = args else {
        return Err(usage());
    };
    let shown = args.get(3).unwrap_or(a);
    // Every side is read (and every failure printed) before any is refused.
    let (ob, ab, bb) = (read(o), read(a), read(b));
    let (ob, ab, bb) = (ob?, ab?, bb?);
    let (od, ad, bd) = (parse(shown, &ob), parse(shown, &ab), parse(shown, &bb));
    let (od, ad, bd) = (od?, ad?, bd?);
    let merged = format::merge3(&od, &ad, &bd);
    write(a, merged.text.as_bytes())?;
    if merged.conflicts > 0 {
        eprintln!("{shown}: {} conflicting record(s)", merged.conflicts);
        return Ok(DESIGN_ERROR);
    }
    Ok(OK)
}

/// `diff A B`: one line per added (`+`), removed (`-`) or changed (`~`) record, a changed
/// one with the parts and attribute keys that differ. Exit 1 when the files differ, like
/// `diff(1)`.
pub fn diff(args: &[String]) -> Exit {
    let [a, b] = args else {
        return Err(usage());
    };
    let (ab, bb) = (read(a), read(b));
    let (ab, bb) = (ab?, bb?);
    let (ad, bd) = (parse(a, &ab), parse(b, &bb));
    let (ad, bd) = (ad?, bd?);
    let d = format::diff(&ad, &bd);
    let mut out = String::new();
    for c in &d.changes {
        let (sign, doc, r) = match c.kind {
            format::ChangeKind::Added => ('+', &bd, c.b),
            format::ChangeKind::Removed => ('-', &ad, c.a),
            format::ChangeKind::Changed => ('~', &bd, c.b),
        };
        let (k1, k2) = doc.record_key(r);
        out.push(sign);
        out.push(' ');
        out.push_str(doc.record_word(r));
        for k in [k1, k2].into_iter().filter(|k| !k.is_empty()) {
            out.push(' ');
            out.push_str(k);
        }
        if c.kind == format::ChangeKind::Changed {
            push_changed_parts(&mut out, c.parts, &d.keys[c.keys.start as usize..c.keys.end as usize]);
        }
        out.push('\n');
    }
    print!("{out}");
    Ok(if d.changes.is_empty() { OK } else { DESIGN_ERROR })
}

/// `git-setup`: configures this clone's merge driver (git never reads driver commands from
/// the repository, for security) and adds the `.gitattributes` lines it lacks.
pub fn git_setup(args: &[String]) -> Exit {
    if !args.is_empty() {
        return Err(usage());
    }
    let settings = [
        ("merge.schemify.name", "Schemify semantic merge"),
        ("merge.schemify.driver", "schemify merge-driver %O %A %B %P"),
    ];
    for (key, value) in settings {
        let status = std::process::Command::new("git")
            .args(["config", key, value])
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            eprintln!("git config {key} failed (is this a git clone?)");
            return Err(USAGE_ERROR);
        }
    }
    let path = Path::new(".gitattributes");
    let mut text = std::fs::read_to_string(path).unwrap_or_default();
    let before = text.len();
    for ext in ["shprim", "shcomp", "shbench"] {
        let line = format!("*.{ext} merge=schemify");
        if text.lines().any(|l| l.trim() == line) {
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&line);
        text.push('\n');
    }
    if text.len() != before
        && let Err(e) = std::fs::write(path, text)
    {
        eprintln!(".gitattributes: {e}");
        return Err(USAGE_ERROR);
    }
    println!("merge driver configured; commit .gitattributes so clones share it");
    Ok(OK)
}

// ---- private ----

/// Reads a file; an I/O error is printed and becomes [`USAGE_ERROR`].
fn read(path: &str) -> Result<Vec<u8>, u8> {
    platform::read(Path::new(path)).map_err(|e| {
        eprintln!("{e}");
        USAGE_ERROR
    })
}

/// Parses `bytes` read from `path`; diagnostics are printed as `path:line:col: …` and the
/// failure becomes [`DESIGN_ERROR`].
fn parse<'b>(path: &str, bytes: &'b [u8]) -> Result<format::Document<'b>, u8> {
    format::parse(bytes).map_err(|e| {
        for d in &e.diagnostics {
            eprintln!("{path}:{d}");
        }
        DESIGN_ERROR
    })
}

/// Writes `bytes` to `path` atomically; an I/O error is printed and becomes [`USAGE_ERROR`].
fn write(path: &str, bytes: &[u8]) -> Result<(), u8> {
    write_atomic(Path::new(path), bytes).map_err(|e| {
        eprintln!("{e}");
        USAGE_ERROR
    })
}

/// Appends ` [fields] [text] [comments] [attrs: k1, k2]` for the parts a changed record
/// differs in.
fn push_changed_parts(out: &mut String, parts: format::Parts, keys: &[&str]) {
    let names = [
        (format::Parts::FIELDS, "fields"),
        (format::Parts::VERBATIM, "text"),
        (format::Parts::COMMENTS, "comments"),
    ];
    for (_, name) in names.into_iter().filter(|(p, _)| parts.contains(*p)) {
        out.push_str(" [");
        out.push_str(name);
        out.push(']');
    }
    if !keys.is_empty() {
        out.push_str(" [attrs: ");
        out.push_str(&keys.join(", "));
        out.push(']');
    }
}
