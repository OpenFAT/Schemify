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
//! - `pub fn run_host(Dialect, &Project, &mut Tools) -> Result<Option<String>, String>` (native)

/// The simulators Schemify starts (run spec §5), in detection order.
pub use schemify_run::SIMULATORS;

#[cfg(not(target_arch = "wasm32"))]
use std::env;
#[cfg(not(target_arch = "wasm32"))]
use std::mem;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use schemify_config::Table;
#[cfg(not(target_arch = "wasm32"))]
use schemify_config::{SimKind, valid_host};
use schemify_doc::Project;
use schemify_elab::Dialect;
#[cfg(not(target_arch = "wasm32"))]
use schemify_run::{SimCommand, Tools};

/// The dialect a request netlists and simulates for (run spec §5): `explicit`
/// (`--dialect`, the Netlist dialect menu), else the simulator the PDK pins (its tech
/// map's `simulator`, pdk spec §6), else the user config's `schemify.opt.simulator`, else
/// the first of [`SIMULATORS`] installed, else ESPice (the default simulator, user decision
/// 2026-10-25).
///
/// # Errors
///
/// The tech map pins a word that names no simulator.
///
/// # Panics
///
/// Panics if a fallback lands outside [`SIMULATORS`] (a bug here).
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
    let chosen = match user.opts.simulator {
        Some(k) => SIMULATORS
            .into_iter()
            .find(|d| d.as_str() == k.as_str())
            .unwrap_or(Dialect::Espice),
        None => detect(user).unwrap_or(Dialect::Espice),
    };
    assert!(SIMULATORS.contains(&chosen), "the fallbacks are simulators");
    Ok(chosen)
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
    match word.parse() {
        Ok(d) if SIMULATORS.contains(&d) || d == Dialect::Hspice => Ok(Some((map.pdk.as_str(), d))),
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
/// configured or default program is not found here (a PDK that names a `host` runs it
/// there, [`run_host`]).
///
/// # Panics
///
/// Panics if the choice lands outside [`SIMULATORS`] (a bug here).
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
        && project.techmap().is_none_or(|m| m.host.is_none())
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
/// `args` keeps the default arguments; a row with a `host` goes to [`Tools::hosts`], without
/// `path` the default program (on the host's `PATH`, config spec §3).
///
/// # Panics
///
/// Panics if a simulator row ends up without a program (the config refuses an empty
/// `path`, and every default names one), or a host row is a compiler's.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn tools(user: &Table) -> Tools {
    let mut t = Tools::default();
    assert_eq!(
        t.sims.len(),
        SIMULATORS.len(),
        "one program row per simulator"
    );
    for (k, d) in SIMULATORS.into_iter().enumerate() {
        let Some(s) = user.sim(kind(d)) else {
            continue;
        };
        // A row without `args` keeps the default row's.
        let default_args = mem::take(&mut t.sims[k].args);
        t.sims[k] = SimCommand {
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
    for (k, host, s) in &user.hosts {
        let d = SIMULATORS
            .into_iter()
            .find(|d| d.as_str() == k.as_str())
            .unwrap_or_else(|| panic!("{} on {host} is not a simulator", k.as_str()));
        let default = SimCommand::default_for(d);
        let cmd = SimCommand {
            path: if s.path.is_empty() {
                default.path
            } else {
                s.path.clone()
            },
            args: if s.args.is_empty() {
                default.args
            } else {
                s.args.clone()
            },
            env: s.env.clone(),
        };
        t.hosts.push((d, host.clone(), cmd));
    }
    assert_eq!(
        t.hosts.len(),
        user.hosts.len(),
        "one host row per config row"
    );
    assert!(
        t.sims
            .iter()
            .chain(t.hosts.iter().map(|h| &h.2))
            .all(|s| !s.path.is_empty())
    );
    t
}

/// The ssh destination a run of simulator `run` goes to (run spec §5): the host the
/// project's PDK names (tech map `host`, pdk spec §6), with `tools` pointed at the user
/// config's row for `run` there ([`Tools::on_host`]); `None`, `tools` unchanged, without one.
///
/// # Errors
///
/// The host is not an ssh destination ([`valid_host`]), or the user config names no `sim`
/// row for `run` on it: a refused run, never a guess (D7).
///
/// # Panics
///
/// Panics if `run` is not one of [`SIMULATORS`].
#[cfg(not(target_arch = "wasm32"))]
pub fn run_host(
    run: Dialect,
    project: &Project,
    tools: &mut Tools,
) -> Result<Option<String>, String> {
    assert!(
        SIMULATORS.contains(&run),
        "{} is not a simulator",
        run.as_str()
    );
    let Some(map) = project.techmap() else {
        return Ok(None);
    };
    let Some(host) = map.host.as_deref() else {
        return Ok(None);
    };
    if !valid_host(host) {
        return Err(format!(
            "{} names host `{host}`, which is not an ssh destination ([user@]host)",
            map.pdk
        ));
    }
    if !tools.on_host(run, host) {
        return Err(format!(
            "{} runs on `{host}`, where the user config names no `sim {}` row \
             (schemify.sim with host = \"{host}\")",
            map.pdk,
            run.as_str()
        ));
    }
    assert!(tools.hosts.iter().any(|h| h.0 == run && h.1 == host));
    Ok(Some(host.to_owned()))
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
    assert!(SIMULATORS.contains(&d), "{} is not a simulator", d.as_str());
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
    env::var_os("PATH")
        .is_some_and(|path| env::split_paths(&path).any(|dir| dir.join(&exe).is_file()))
}

/// The user config's program row of simulator `d` (one of [`SIMULATORS`]).
#[cfg(not(target_arch = "wasm32"))]
fn kind(d: Dialect) -> SimKind {
    // By word, not by position: `SIMULATORS` is in detection order, the config's rows in
    // its own.
    assert!(SIMULATORS.contains(&d), "{} is not a simulator", d.as_str());
    SimKind::SIMULATORS
        .into_iter()
        .find(|k| k.as_str() == d.as_str())
        .unwrap_or_else(|| panic!("the config has no row for {}", d.as_str()))
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
                kind(d).as_str(),
                d.as_str(),
                "{} maps to its own row",
                d.as_str()
            );
        }
        let user = Table::default();
        let t = tools(&user);
        for d in SIMULATORS {
            assert_eq!(t.sim(d).path, SimCommand::default_for(d).path);
        }
    }

    /// A `sim` row with a `host` reaches [`Tools::hosts`] with what it leaves out taken from
    /// the defaults (config spec §3: no `path` is the default program on the host's `PATH`;
    /// run spec §5: no `args` keeps the default arguments), and [`Tools::on_host`] runs it;
    /// a local row names no host.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn host_rows_reach_tools_with_the_defaults_they_leave_out() {
        use schemify_config::Sim;
        let zig = vec![("ZIG".to_owned(), "/z/zig".to_owned())];
        let mut user = Table::default();
        user.sims[SimKind::Espice as usize] = Some(Sim {
            path: "/opt/espice".into(),
            args: Vec::new(),
            env: Vec::new(),
        });
        assert!(tools(&user).hosts.is_empty(), "a local row names no host");

        user.hosts.push((
            SimKind::Espice,
            "pdk@server".into(),
            Sim {
                path: String::new(),
                args: Vec::new(),
                env: zig.clone(),
            },
        ));
        user.hosts.push((
            SimKind::Spectre,
            "server".into(),
            Sim {
                path: "/eda/spectre".into(),
                args: vec!["{netlist}".into()],
                env: Vec::new(),
            },
        ));
        let mut t = tools(&user);
        let espice = SimCommand {
            env: zig,
            ..SimCommand::default_for(Dialect::Espice)
        };
        assert_eq!(
            t.hosts,
            [
                (Dialect::Espice, "pdk@server".to_owned(), espice.clone()),
                (
                    Dialect::Spectre,
                    "server".to_owned(),
                    SimCommand {
                        path: "/eda/spectre".into(),
                        args: vec!["{netlist}".into()],
                        env: Vec::new(),
                    }
                ),
            ]
        );
        assert_eq!(
            t.sim(Dialect::Espice).path,
            "/opt/espice",
            "the local row stays"
        );
        assert!(!t.on_host(Dialect::Espice, "server"), "no espice row there");
        assert!(t.on_host(Dialect::Espice, "pdk@server"));
        assert_eq!(*t.sim(Dialect::Espice), espice);
    }
}
