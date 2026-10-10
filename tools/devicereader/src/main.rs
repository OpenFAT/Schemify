//! `devicereader`: the xschem importer as a standalone binary (also `schemify import`).

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::ExitCode::from(schemify_devicereader::run(&args))
}
