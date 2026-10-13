//! `schemify-interop`: the interop plugin's conversions as a native command line
//! (`interop::cli`; built with `--features cli`).

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::ExitCode::from(interop::cli::run(&args))
}
