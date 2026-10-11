//! `schemify-gui`: the editor (D39), started directly or by `schemify edit`.
//!
//! `schemify-gui [edit] [<project-dir>|<cell-file>] [--no-default-ui]`
//! Exit codes: 0 closed normally, 2 usage or startup errors.

use std::process::ExitCode;

const USAGE: &str = "\
usage: schemify-gui [<project-dir>|<cell-file>] [--no-default-ui]
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `cargo run -p schemify-app -- edit <path>` keeps working: the word is optional.
    let rest: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|&(i, a)| !(i == 0 && a == "edit"))
        .map(|(_, a)| a)
        .collect();
    let no_default_ui = rest.iter().any(|a| *a == "--no-default-ui");
    let unknown = rest
        .iter()
        .any(|a| a.starts_with('-') && *a != "--no-default-ui");
    let paths: Vec<&&String> = rest.iter().filter(|a| !a.starts_with('-')).collect();
    if paths.len() > 1 || unknown {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }
    let opts = schemify_app::editor::Options {
        no_default_ui,
        path: paths.first().map(|p| std::path::PathBuf::from(p.as_str())),
        seed: schemify_app::platform::seed(),
        ..schemify_app::editor::Options::default()
    };
    match schemify_app::editor::run(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("schemify-gui: {e}");
            ExitCode::from(2)
        }
    }
}
