//! The external programs a runner starts (run spec §5, D7): the user config's `sim` rows in,
//! one command line per simulator (and per simulator on another host), the compilers and the
//! ssh client out. Never from a document.
//!
//! Contents:
//! - `pub struct SimCommand` (`default_for`)
//! - `pub struct Tools` (`set_sims`, `sim`, `on_host`; `impl Default`)

use crate::SIMULATORS;
use schemify_elab::Dialect;

/// A simulator command line from the user config (`sim <name>` with `path`, `args`,
/// `env`; config spec §3). `{netlist}` and `{raw}` in `args` become the job's file names;
/// the working directory is the job directory; the process environment is inherited with
/// `env` set on top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimCommand {
    /// Program, looked up on `PATH` when it has no directory part.
    pub path: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Variables set for the simulator (ESPice's `ZIG`, the Zig its `.hdl` compiles with).
    pub env: Vec<(String, String)>,
}

/// External programs a runner may start, all from the user config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tools {
    /// Each simulator's command line, in [`SIMULATORS`] order; a runner starts the one
    /// of its [`RunnerConfig::dialect`](crate::RunnerConfig::dialect).
    pub sims: [SimCommand; SIMULATORS.len()],
    /// The user config's simulators on other machines (`sim` rows with a `host`, config spec
    /// §3): (simulator, ssh destination, its command line there). [`Tools::on_host`] picks
    /// one for a run whose PDK is on that host.
    pub hosts: Vec<(Dialect, String, SimCommand)>,
    /// OpenVAF (`<src> -o <out>`).
    pub openvaf: String,
    /// Icarus Verilog (`-o <out> <src>`).
    pub iverilog: String,
    /// The ssh client remote jobs run through (run spec §5): `ssh` on `PATH`.
    pub ssh: String,
}

impl SimCommand {
    /// Returns the default command line of simulator `d` (run spec §5): batch mode, the
    /// job's netlist in, a raw file out the reader loads.
    ///
    /// - ngspice `ngspice -b -r {raw} {netlist}` (D15);
    /// - ESPice `espice -b --tokenizer=ngspice -r {raw} {netlist}` (the ngspice deck
    ///   syntax the espice dialect writes; ESPice's default `--format` is ngspice binary);
    /// - Xyce `Xyce -r {raw} {netlist}` (a binary raw file);
    /// - LTspice `ltspice -b {netlist}` (it writes `netlist.raw` beside the deck);
    /// - Spectre `spectre -format nutbin -raw {raw} {netlist}` (Nutmeg binary, not PSF).
    ///
    /// # Panics
    ///
    /// Panics if `d` is not one of [`SIMULATORS`].
    #[must_use]
    pub fn default_for(d: Dialect) -> SimCommand {
        assert!(SIMULATORS.contains(&d), "{} is not a simulator", d.as_str());
        let (path, args): (&str, &[&str]) = match d {
            Dialect::Ngspice => ("ngspice", &["-b", "-r", "{raw}", "{netlist}"]),
            Dialect::Espice => (
                "espice",
                &["-b", "--tokenizer=ngspice", "-r", "{raw}", "{netlist}"],
            ),
            Dialect::Xyce => ("Xyce", &["-r", "{raw}", "{netlist}"]),
            Dialect::Ltspice => ("ltspice", &["-b", "{netlist}"]),
            Dialect::Spectre => (
                "spectre",
                &["-format", "nutbin", "-raw", "{raw}", "{netlist}"],
            ),
            Dialect::Cdl | Dialect::Verilog | Dialect::Hspice => unreachable!("checked above"),
        };
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        assert!(
            args.iter().any(|a| a == "{netlist}"),
            "the job's netlist goes in"
        );
        SimCommand {
            path: path.to_owned(),
            args,
            env: Vec::new(),
        }
    }
}

impl Tools {
    /// Gives every simulator the command line `cmd` (a test harness's fake simulator, the
    /// editor's `--sim`).
    pub fn set_sims(&mut self, cmd: &SimCommand) {
        for s in &mut self.sims {
            s.clone_from(cmd);
        }
        debug_assert!(
            self.sims.iter().all(|s| s == cmd),
            "every simulator runs `cmd`"
        );
    }

    /// Returns the command line of simulator `d`.
    ///
    /// # Panics
    ///
    /// Panics if `d` is not one of [`SIMULATORS`].
    #[must_use]
    pub fn sim(&self, d: Dialect) -> &SimCommand {
        let Some(k) = SIMULATORS.iter().position(|s| *s == d) else {
            panic!("{} is not a simulator", d.as_str());
        };
        &self.sims[k]
    }

    /// Makes simulator `d` start the command line the user config names for it on `host`
    /// (a [`Tools::hosts`] row); returns `false`, changing nothing, when it names none. Only
    /// the user's own config names hosts and their programs (D7), so a run whose PDK is on
    /// a host without a row is refused rather than run with a guess.
    ///
    /// # Panics
    ///
    /// Panics if `d` is not one of [`SIMULATORS`], or the row has no program.
    #[must_use]
    pub fn on_host(&mut self, d: Dialect, host: &str) -> bool {
        let Some(k) = SIMULATORS.iter().position(|s| *s == d) else {
            panic!("{} is not a simulator", d.as_str());
        };
        let row = self.hosts.iter().find(|r| r.0 == d && r.1 == host);
        let Some((_, _, cmd)) = row else {
            return false;
        };
        self.sims[k] = cmd.clone();
        assert!(
            !self.sims[k].path.is_empty(),
            "a host row names its program"
        );
        true
    }
}

impl Default for Tools {
    fn default() -> Tools {
        Tools {
            sims: SIMULATORS.map(SimCommand::default_for),
            hosts: Vec::new(),
            openvaf: "openvaf".to_owned(),
            iverilog: "iverilog".to_owned(),
            ssh: "ssh".to_owned(),
        }
    }
}
