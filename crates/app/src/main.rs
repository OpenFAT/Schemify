//! `schemify`: CLI entry point and the spines (ARCHITECTURE.md § Spines).
//!
//! Exit codes (D27): 0 success, 1 design errors (violations, merge conflicts,
//! non-canonical files under `--check`), 2 usage or I/O errors.

use std::path::Path;
use std::process::ExitCode;

use schemify_format as format;

const USAGE: &str = "\
usage: schemify <command> [args]

commands:
  fmt [--check] <file>...              rewrite files in canonical form
  merge-driver <O> <A> <B> [<path>]    git merge driver (writes the result to A)
  diff <A> <B>                         record-level semantic diff of two files
  git-setup                            register the merge driver in this clone
  import <xschem-dir> <out-dir>        convert an xschem library (designs or PDK devices)
  netlist <cell> [--dialect D] [--config C] [--corner K] [-o FILE] [--allow-errors]
  check <cell>                         report violations
";

const OK: u8 = 0;
const DESIGN_ERROR: u8 = 1;
const USAGE_ERROR: u8 = 2;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::from(USAGE_ERROR);
    };
    let rest = &args[1..];
    // Spine selection is the only branch on which work happens (codebase-clean-code rule 1).
    let code = match command.as_str() {
        "fmt" => fmt(rest),
        "merge-driver" => merge_driver(rest),
        "diff" => diff(rest),
        "git-setup" => git_setup(rest),
        "import" | "netlist" | "check" => {
            eprintln!("schemify {command}: not implemented yet");
            USAGE_ERROR
        }
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            OK
        }
        _ => {
            eprint!("unknown command `{command}`\n{USAGE}");
            USAGE_ERROR
        }
    };
    ExitCode::from(code)
}

/// Reads a file, mapping I/O errors to a printed message.
fn read(path: &str) -> Result<Vec<u8>, u8> {
    std::fs::read(path).map_err(|e| {
        eprintln!("{path}: {e}");
        USAGE_ERROR
    })
}

/// Parses `bytes` from `path`, printing diagnostics as `path:line:col: …` on failure.
fn parse<'b>(path: &str, bytes: &'b [u8]) -> Result<format::Document<'b>, u8> {
    format::parse(bytes).map_err(|e| {
        for d in &e.diagnostics {
            eprintln!("{path}:{d}");
        }
        DESIGN_ERROR
    })
}

/// `fmt`: parse → canonical write; rewrite in place unless `--check`.
fn fmt(args: &[String]) -> u8 {
    let check = args.iter().any(|a| a == "--check");
    let files: Vec<&String> = args.iter().filter(|a| *a != "--check").collect();
    if files.is_empty() {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    }
    let mut worst = OK;
    for path in files {
        let bytes = match read(path) {
            Ok(b) => b,
            Err(c) => return c,
        };
        let doc = match parse(path, &bytes) {
            Ok(d) => d,
            Err(c) => {
                worst = worst.max(c);
                continue;
            }
        };
        let canonical = format::write(&doc);
        if canonical.as_bytes() == bytes.as_slice() {
            continue;
        }
        if check {
            eprintln!("{path}: not in canonical form");
            worst = worst.max(DESIGN_ERROR);
        } else if let Err(e) = write_atomic(Path::new(path), canonical.as_bytes()) {
            eprintln!("{path}: {e}");
            return USAGE_ERROR;
        }
    }
    worst
}

/// `merge-driver O A B [P]`: semantic 3-way merge (format spec §8); the result
/// replaces A, as git expects. Exit 1 when conflict blocks remain.
fn merge_driver(args: &[String]) -> u8 {
    let [o, a, b, ..] = args else {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    };
    let shown = args.get(3).unwrap_or(a);
    let (ob, ab, bb) = match (read(o), read(a), read(b)) {
        (Ok(o), Ok(a), Ok(b)) => (o, a, b),
        _ => return USAGE_ERROR,
    };
    // A side that doesn't parse can't be merged by record: leave A untouched so
    // git reports the conflict and the user resolves it by hand.
    let (od, ad, bd) = match (parse(shown, &ob), parse(shown, &ab), parse(shown, &bb)) {
        (Ok(o), Ok(a), Ok(b)) => (o, a, b),
        _ => return DESIGN_ERROR,
    };
    let merged = format::merge3(&od, &ad, &bd);
    if let Err(e) = write_atomic(Path::new(a), merged.text.as_bytes()) {
        eprintln!("{a}: {e}");
        return USAGE_ERROR;
    }
    if merged.conflicts > 0 {
        eprintln!("{shown}: {} conflicting record(s)", merged.conflicts);
        return DESIGN_ERROR;
    }
    OK
}

/// `diff A B`: one line per added (`+`), removed (`-`) or changed (`~`) record.
/// Exit 1 when the files differ, like `diff(1)`.
fn diff(args: &[String]) -> u8 {
    let [a, b] = args else {
        eprint!("{USAGE}");
        return USAGE_ERROR;
    };
    let (ab, bb) = match (read(a), read(b)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return USAGE_ERROR,
    };
    let (ad, bd) = match (parse(a, &ab), parse(b, &bb)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return DESIGN_ERROR,
    };
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
        for k in [k1, k2] {
            if !k.is_empty() {
                out.push(' ');
                out.push_str(k);
            }
        }
        if c.kind == format::ChangeKind::Changed {
            let what = [
                (format::Parts::FIELDS, "fields"),
                (format::Parts::VERBATIM, "text"),
                (format::Parts::COMMENTS, "comments"),
            ];
            for (part, name) in what {
                if c.parts.contains(part) {
                    out.push_str(" [");
                    out.push_str(name);
                    out.push(']');
                }
            }
            let keys = &d.keys[c.keys.start as usize..c.keys.end as usize];
            if !keys.is_empty() {
                out.push_str(" [attrs: ");
                out.push_str(&keys.join(", "));
                out.push(']');
            }
        }
        out.push('\n');
    }
    print!("{out}");
    if d.changes.is_empty() {
        OK
    } else {
        DESIGN_ERROR
    }
}

/// `git-setup`: configures this clone's merge driver (git never reads driver
/// commands from the repo, for security) and adds the `.gitattributes` lines.
fn git_setup(args: &[String]) -> u8 {
    if !args.is_empty() {
        eprint!("{USAGE}");
        return USAGE_ERROR;
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
            return USAGE_ERROR;
        }
    }
    let path = Path::new(".gitattributes");
    let mut text = std::fs::read_to_string(path).unwrap_or_default();
    let before = text.len();
    for ext in ["shprim", "shcomp", "shbench"] {
        let line = format!("*.{ext} merge=schemify");
        if !text.lines().any(|l| l.trim() == line) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&line);
            text.push('\n');
        }
    }
    if text.len() != before
        && let Err(e) = std::fs::write(path, text)
    {
        eprintln!(".gitattributes: {e}");
        return USAGE_ERROR;
    }
    println!("merge driver configured; commit .gitattributes so clones share it");
    OK
}

/// Writes `bytes` to a sibling temp file, then renames it over `path`, so a
/// crash never leaves a half-written design file (edits spec §5).
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".schemify-tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
