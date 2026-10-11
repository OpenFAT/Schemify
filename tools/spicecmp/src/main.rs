//! `spicecmp [--alias FROM=TO]... [--bind-free] [--quiet] A B` — semantic netlist diff
//! (`--bind-free`: `Options::bind_free`, A is the reference).
//! Exit 0: equivalent; 1: different; 2: usage or I/O error.

use std::process::ExitCode;

use schemify_spicecmp::{Options, compare};

const USAGE: &str =
    "usage: spicecmp [--alias FROM=TO]... [--bind-free] [--quiet] <reference.spice> <b.spice>";

fn main() -> ExitCode {
    let mut opts = Options::default();
    let mut quiet = false;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quiet" | "-q" => quiet = true,
            "--bind-free" => opts.bind_free = true,
            "--alias" => match args.next().as_deref().and_then(|a| a.split_once('=')) {
                Some((from, to)) => opts.path_aliases.push((from.to_string(), to.to_string())),
                None => return usage("--alias needs FROM=TO"),
            },
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => files.push(arg),
        }
    }
    let [a, b] = files.as_slice() else {
        return usage("need exactly two files");
    };
    let read = |p: &str| std::fs::read(p).map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let (ta, tb) = match (read(a), read(b)) {
        (Ok(ta), Ok(tb)) => (ta, tb),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("spicecmp: {e}");
            return ExitCode::from(2);
        }
    };
    let report = compare(&ta, &tb, &opts);
    if !quiet || !report.is_equal() {
        print!("{report}");
    }
    if report.is_equal() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn usage(why: &str) -> ExitCode {
    eprintln!("spicecmp: {why}\n{USAGE}");
    ExitCode::from(2)
}
