//! The simulator choice (run spec §5; pdk spec §6): a request, the project's tech map and the
//! user config → the dialect a run netlists for, the simulator it starts, and the programs
//! the runner calls. Natively it looks for programs on `PATH`; the web starts none (D16).
//!
//! Contents:
//! - `pub use schemify_run::SIMULATORS`
//! - `pub fn sim_dialect(Option<Dialect>, &Project, &Table) -> Result<Dialect, String>`
//! - `pub fn pin(&Project) -> Result<Option<(&str, Dialect)>, String>`
//! - `pub fn run_dialect(Dialect, &Project, &Table) -> Result<Dialect, String>`
//! - `pub fn tools(&Table) -> Tools` (native)

/// The simulators Schemify starts (run spec §5), in detection order.
pub use schemify_run::SIMULATORS;

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use schemify_config::Table;
use schemify_doc::Project;
use schemify_elab::Dialect;

use super::dialect_of;

/// The dialect a request netlists and simulates for (run spec §5): `explicit`
/// (`--dialect`, the Netlist dialect menu), else the simulator the PDK pins (its tech
/// map's `simulator`, pdk spec §6), else the user config's `schemify.opt.simulator`, else
/// the first of [`SIMULATORS`] installed, else ESPice (the default simulator, user decision
/// 2026-10-25).
///
/// # Errors
///
/// The tech map pins a word that names no simulator.
pub fn sim_dialect(
    explicit: Option<Dialect>,
    project: &Project,
    user: &Table,
) -> Result<Dialect, String> {
    if let Some(d) = explicit {
        return Ok(d);
    }
    if let Some((_, d)) = pin(project)? {
        return Ok(d);
    }
    Ok(match user.opts.simulator {
        Some(k) => SIMULATORS
            .into_iter()
            .find(|d| d.as_str() == k.word())
            .unwrap_or(Dialect::Espice),
        None => detect(user).unwrap_or(Dialect::Espice),
    })
}

/// The simulator the project's PDK pins: (PDK name, dialect); `None` without a tech map
/// or without its `simulator`.
///
/// # Errors
///
/// The pinned word names neither one of [`SIMULATORS`] nor `hspice`.
pub fn pin(project: &Project) -> Result<Option<(&str, Dialect)>, String> {
    let Some(map) = project.techmap() else {
        return Ok(None);
    };
    let Some(word) = map.simulator.as_deref() else {
        return Ok(None);
    };
    match dialect_of(word) {
        Some(d) if SIMULATORS.contains(&d) || d == Dialect::Hspice => {
            Ok(Some((map.pdk.as_str(), d)))
        }
        _ => Err(format!(
            "{} pins simulator `{word}`, which is not a simulator \
             (ngspice espice xyce ltspice spectre hspice)",
            map.pdk
        )),
    }
}

/// The simulator a run netlisted for `chosen` starts (run spec §5): `chosen` when it is
/// one of [`SIMULATORS`]; any other dialect runs ngspice unless the PDK pins a
/// simulator, which never falls back.
///
/// # Errors
///
/// The PDK pins a simulator Schemify cannot start (HSPICE), or (native) one whose
/// configured or default program is not found.
pub fn run_dialect(chosen: Dialect, project: &Project, user: &Table) -> Result<Dialect, String> {
    let pin = pin(project)?;
    let run = match (chosen, pin) {
        (d, _) if SIMULATORS.contains(&d) => d,
        (_, None) => Dialect::Ngspice,
        (d, Some((pdk, _))) => {
            return Err(format!(
                "{pdk} pins simulator {}, which Schemify cannot start \
                 (ngspice espice xyce ltspice spectre)",
                d.as_str()
            ));
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((pdk, p)) = pin
        && p == run
        && !installed(run, user)
    {
        return Err(format!(
            "{pdk} pins simulator {}, which is not installed (name its program with schemify.sim)",
            run.as_str()
        ));
    }
    #[cfg(target_arch = "wasm32")]
    let _ = user;
    assert!(SIMULATORS.contains(&run));
    Ok(run)
}

/// The external programs of run spec §5 from the user config's `schemify.sim` rows; the
/// defaults for rows it does not set (documents never name a program, D7). A row without
/// `args` keeps the default arguments.
#[cfg(not(target_arch = "wasm32"))]
pub fn tools(user: &Table) -> schemify_run::Tools {
    use schemify_config::SimKind;
    let mut t = schemify_run::Tools::default();
    for (k, d) in SIMULATORS.into_iter().enumerate() {
        let Some(s) = user.sim(kind(d)) else {
            continue;
        };
        // A row without `args` keeps the default row's.
        let default_args = std::mem::take(&mut t.sims[k].args);
        t.sims[k] = schemify_run::SimCommand {
            path: s.path.clone(),
            args: if s.args.is_empty() {
                default_args
            } else {
                s.args.clone()
            },
            env: s.env.clone(),
        };
    }
    if let Some(s) = user.sim(SimKind::Openvaf) {
        t.openvaf.clone_from(&s.path);
    }
    if let Some(s) = user.sim(SimKind::Iverilog) {
        t.iverilog.clone_from(&s.path);
    }
    assert!(t.sims.iter().all(|s| !s.path.is_empty()));
    t
}

// ---- private ----

/// The first of [`SIMULATORS`] whose program (the config row's `path`, else the default)
/// is found; `None` on the web, which starts no simulator.
fn detect(user: &Table) -> Option<Dialect> {
    #[cfg(not(target_arch = "wasm32"))]
    return SIMULATORS.into_iter().find(|d| installed(*d, user));
    #[cfg(target_arch = "wasm32")]
    {
        let _ = user;
        None
    }
}

/// Whether simulator `d`'s program exists: a path with a directory part as a file, a bare
/// name in a `PATH` directory (with `.exe` on Windows). Runs no program.
#[cfg(not(target_arch = "wasm32"))]
fn installed(d: Dialect, user: &Table) -> bool {
    let prog = tools(user).sim(d).path.clone();
    let p = Path::new(&prog);
    if p.components().count() > 1 {
        return p.is_file();
    }
    let exe = if cfg!(windows) {
        format!("{prog}.exe")
    } else {
        prog
    };
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(&exe).is_file()))
}

/// The user config's program row of simulator `d` (one of [`SIMULATORS`]).
#[cfg(not(target_arch = "wasm32"))]
fn kind(d: Dialect) -> schemify_config::SimKind {
    // By word, not by position: `SIMULATORS` is in detection order, the config's rows in
    // its own.
    assert!(SIMULATORS.contains(&d), "{} is not a simulator", d.as_str());
    let k = schemify_config::SimKind::SIMULATORS
        .into_iter()
        .find(|k| k.word() == d.as_str())
        .unwrap_or_else(|| panic!("the config has no row for {}", d.as_str()));
    assert_eq!(k.word(), d.as_str());
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ESPice is detected first and is the last fallback (user decision 2026-10-25); every
    /// simulator still finds its own config row, by word, whatever the detection order.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn espice_leads_and_every_simulator_has_its_config_row() {
        assert_eq!(SIMULATORS[0], Dialect::Espice, "espice is detected first");
        for d in SIMULATORS {
            assert_eq!(
                kind(d).word(),
                d.as_str(),
                "{} maps to its own row",
                d.as_str()
            );
        }
        let user = Table::default();
        let t = tools(&user);
        for d in SIMULATORS {
            assert_eq!(t.sim(d).path, schemify_run::SimCommand::default_for(d).path);
        }
    }
}
