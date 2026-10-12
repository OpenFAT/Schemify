//! Bench tables as a run reads them (format spec §4 ranks 4 and 6, run spec §1): vars
//! with bounds and Monte Carlo distributions, corners, configs, sweeps, Monte Carlo and
//! optimize records, outputs and specs.
//!
//! Six questions: in, one bench [`Document`]; out, one [`Plan`]. Every table holds at
//! most [`TABLE_MAX`] rows (doc's bench capacity), so these are small AoS rows read
//! together by name (sweep name → var + values), not hot columns. Lifetime: one Run.
//! Read once, then shared read-only by expansion, results and the optimizer.

use schemify_doc::{Document, RecordKind, Span};
use schemify_expr::{Mode, Program, Scratch, Value};
use std::fmt;

/// Most rows of one bench table (doc capacity `2^12`).
pub const TABLE_MAX: usize = 1 << 12;
/// Most points one sweep expands to.
pub const SWEEP_POINTS_MAX: u32 = 1 << 16;
/// Most samples one Monte Carlo record asks for.
pub const SAMPLES_MAX: u32 = 1 << 20;
/// Most evaluations one optimize record allows.
pub const BUDGET_MAX: u32 = 1 << 16;

/// A bench record that cannot run as written, or a run selection naming what the bench
/// lacks. `record` is `<kind> <name>` (`sweep rsw`, `select`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanError {
    /// Offending record.
    pub record: String,
    /// What is wrong with it.
    pub detail: String,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.record, self.detail)
    }
}

impl std::error::Error for PlanError {}

pub(crate) fn plan_err(kind: &str, name: &str, detail: impl Into<String>) -> PlanError {
    PlanError {
        record: format!("{kind} {name}"),
        detail: detail.into(),
    }
}

/// One bench var (`var <name> <expr>`): the sweep, Monte Carlo and optimizer variable.
#[derive(Clone, Debug, PartialEq)]
pub struct Var {
    /// Name.
    pub name: String,
    /// Expression text as written (bench scope).
    pub expr: String,
    /// `min` bound expression, if any.
    pub min: Option<String>,
    /// `max` bound expression, if any.
    pub max: Option<String>,
}

/// One `corner` record: its name and `set <var> <expr>` lines in file order (the base a
/// Monte Carlo variation applies to, run spec §2).
#[derive(Clone, Debug, PartialEq)]
pub struct Corner {
    /// Name.
    pub name: String,
    /// `(var, expr)` per `set` line, file order.
    pub sets: Vec<(String, String)>,
}

/// One `sweep` record with its points already expanded to value texts (run spec §2):
/// `values` entries verbatim, `lin`/`log` points as canonical number text.
#[derive(Clone, Debug, PartialEq)]
pub struct Sweep {
    /// Name.
    pub name: String,
    /// The swept var.
    pub var: String,
    /// One expression text per point, in sweep order; never empty when built with the
    /// `sweep` feature.
    pub values: Vec<String>,
}

/// One `montecarlo` record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonteCarlo {
    /// Name.
    pub name: String,
    /// Samples, `1..=SAMPLES_MAX`.
    pub samples: u32,
    /// Seed of the sample streams.
    pub seed: u64,
}

/// Shape of a Monte Carlo variation (`dist <kind> …` on a var, run spec §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DistKind {
    /// Normal: `spread` is the standard deviation.
    Gauss,
    /// Uniform over `±spread`.
    Uniform,
}

/// A var's Monte Carlo variation: `dist gauss|uniform <spread> [rel]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Dist {
    /// The varied var.
    pub var: String,
    /// Shape.
    pub kind: DistKind,
    /// Spread expression text (bench scope); must not reference `var` itself.
    pub spread: String,
    /// `rel`: the spread is relative to the base value (`base·(1 + spread·z)`).
    pub rel: bool,
}

/// What an optimize `goal` asks of its output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Goal {
    /// Minimize the output.
    Min,
    /// Maximize the output.
    Max,
    /// Minimize `|output − target|`.
    Target(f64),
}

/// One `optimize` record.
#[derive(Clone, Debug, PartialEq)]
pub struct Optimize {
    /// Name.
    pub name: String,
    /// `algo` word (`nelder-mead` when absent; checked by the optimizer).
    pub algo: String,
    /// `(output index, goal)` per `goal` line, file order.
    pub goals: Vec<(u16, Goal)>,
    /// Score weight of each goal (`weight <w>`, 1 when absent), parallel to `goals`.
    pub weights: Vec<f64>,
    /// Varied vars, file order; each has constant `min` and `max` bounds.
    pub vary: Vec<String>,
    /// Configs every candidate runs at (`config` lines, file order); empty: the bench
    /// default.
    pub configs: Vec<String>,
    /// Corners every candidate runs at (`corner` lines, file order); empty: the bench
    /// default.
    pub corners: Vec<String>,
    /// Most evaluations, `1..=BUDGET_MAX` (100 when absent).
    pub budget: u32,
    /// Seed of the random draws (1 when absent).
    pub seed: u64,
}

/// One `output <name> <expr>`: a calculator expression over the job's raw file.
#[derive(Clone, Debug)]
pub struct Output {
    /// Name.
    pub name: String,
    /// Expression text.
    pub src: String,
    /// Compiled in [`Mode::Calc`].
    pub prog: Program,
}

/// One `spec <output>` with its optional bounds (scalar mode, bench scope).
#[derive(Clone, Debug)]
pub struct Spec {
    /// Index into [`Plan::outputs`].
    pub output: u16,
    /// `min` bound.
    pub min: Option<Program>,
    /// `max` bound.
    pub max: Option<Program>,
}

/// Everything a run needs from one bench. Named tables are sorted by name (bytewise);
/// vars keep document order.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// Bench vars, document order.
    pub vars: Vec<Var>,
    /// Corners.
    pub corners: Vec<Corner>,
    /// Config names.
    pub configs: Vec<String>,
    /// Sweeps.
    pub sweeps: Vec<Sweep>,
    /// Monte Carlo records.
    pub montecarlos: Vec<MonteCarlo>,
    /// Monte Carlo variations, sorted by var.
    pub dists: Vec<Dist>,
    /// Optimize records.
    pub optimizes: Vec<Optimize>,
    /// Outputs.
    pub outputs: Vec<Output>,
    /// Specs, sorted by output name.
    pub specs: Vec<Spec>,
    /// The bench's `config` header attribute.
    pub default_config: Option<String>,
    /// The bench's `corner` header attribute.
    pub default_corner: Option<String>,
}

impl Plan {
    /// Reads the bench tables of `doc` and checks them: sweep shapes and point counts,
    /// `dist` attributes, Monte Carlo sample counts, optimize goals (outputs exist) and
    /// vary vars (constant bounds), output and spec expressions compile.
    ///
    /// # Errors
    ///
    /// The first record that cannot run as written.
    pub fn read(doc: &Document) -> Result<Plan, PlanError> {
        let s = doc.strings();
        let pool = doc.pool();
        let first = |span: Span, key: &str| -> Option<Vec<&str>> {
            pool.find_attr(s, span, key)
                .map(|v| v.iter().map(|x| s.get(*x)).collect())
        };
        let mut plan = Plan {
            default_config: doc.header_value("config").map(str::to_owned),
            default_corner: doc.header_value("corner").map(str::to_owned),
            ..Plan::default()
        };

        // ---- vars (rank 4, document order) and their `dist`
        let blocks = doc.doc_blocks();
        for r in 0..blocks.kind.len() {
            if blocks.kind[r] != RecordKind::Var {
                continue;
            }
            let name = s.get(blocks.fields[r][0]).to_owned();
            let one =
                |k| first(blocks.attrs[r], k).and_then(|v| v.first().map(|x| (*x).to_owned()));
            if let Some(d) = first(blocks.attrs[r], "dist") {
                plan.dists.push(read_dist(&name, &d)?);
            }
            plan.vars.push(Var {
                expr: s.get(blocks.fields[r][1]).to_owned(),
                min: one("min"),
                max: one("max"),
                name,
            });
        }
        plan.dists.sort_by(|a, b| a.var.cmp(&b.var));

        // ---- corners and configs
        let t = doc.corners();
        for r in 0..t.kind.len() {
            let sets = pool
                .attrs(t.attrs[r])
                .filter(|(k, v)| s.get(*k) == "set" && v.len() == 2)
                .map(|(_, v)| (s.get(v[0]).to_owned(), s.get(v[1]).to_owned()))
                .collect();
            plan.corners.push(Corner {
                name: s.get(t.fields[r][0]).to_owned(),
                sets,
            });
        }
        plan.corners.sort_by(|a, b| a.name.cmp(&b.name));
        let t = doc.configs();
        plan.configs = (0..t.kind.len())
            .map(|r| s.get(t.fields[r][0]).to_owned())
            .collect();
        plan.configs.sort();

        // ---- sweeps
        let t = doc.sweeps();
        for r in 0..t.kind.len() {
            let name = s.get(t.fields[r][0]).to_owned();
            let var = first(t.attrs[r], "var")
                .and_then(|v| v.first().map(|x| (*x).to_owned()))
                .ok_or_else(|| plan_err("sweep", &name, "has no `var`"))?;
            let shapes = [
                first(t.attrs[r], "values"),
                first(t.attrs[r], "lin"),
                first(t.attrs[r], "log"),
            ];
            #[cfg(feature = "sweep")]
            let values = crate::sweep::points(&shapes).map_err(|e| plan_err("sweep", &name, e))?;
            #[cfg(not(feature = "sweep"))]
            let values = {
                let _ = shapes;
                Vec::new()
            };
            plan.sweeps.push(Sweep { name, var, values });
        }
        plan.sweeps.sort_by(|a, b| a.name.cmp(&b.name));

        // ---- Monte Carlo
        let t = doc.montecarlos();
        for r in 0..t.kind.len() {
            let name = s.get(t.fields[r][0]).to_owned();
            let int = |k| first(t.attrs[r], k).and_then(|v| v.first().map(|x| (*x).to_owned()));
            let samples =
                int("samples").ok_or_else(|| plan_err("montecarlo", &name, "has no `samples`"))?;
            let samples = parse_count(&samples, SAMPLES_MAX)
                .map_err(|e| plan_err("montecarlo", &name, format!("samples: {e}")))?;
            let seed = int("seed").map_or(Ok(0), |x| x.parse::<u64>());
            let seed = seed.map_err(|_| plan_err("montecarlo", &name, "seed is not a u64"))?;
            plan.montecarlos.push(MonteCarlo {
                name,
                samples,
                seed,
            });
        }
        plan.montecarlos.sort_by(|a, b| a.name.cmp(&b.name));

        // ---- outputs and specs (before optimize: goals name outputs)
        let t = doc.outputs();
        for r in 0..t.kind.len() {
            let name = s.get(t.fields[r][0]).to_owned();
            let src = s.get(t.fields[r][1]).to_owned();
            let prog = Program::compile(&src, Mode::Calc)
                .map_err(|e| plan_err("output", &name, e.to_string()))?;
            plan.outputs.push(Output { name, src, prog });
        }
        plan.outputs.sort_by(|a, b| a.name.cmp(&b.name));
        assert!(plan.outputs.len() <= TABLE_MAX, "doc caps bench tables");
        let t = doc.specs();
        for r in 0..t.kind.len() {
            let name = s.get(t.fields[r][0]);
            let output = plan
                .output(name)
                .ok_or_else(|| plan_err("spec", name, "names no output"))?;
            let bound = |k| -> Result<Option<Program>, PlanError> {
                first(t.attrs[r], k)
                    .and_then(|v| v.first().copied())
                    .map(|src| Program::compile(src, Mode::Scalar))
                    .transpose()
                    .map_err(|e| plan_err("spec", name, format!("{k}: {e}")))
            };
            plan.specs.push(Spec {
                output,
                min: bound("min")?,
                max: bound("max")?,
            });
        }
        plan.specs.sort_by_key(|sp| sp.output);

        // ---- optimize
        let t = doc.optimizes();
        for r in 0..t.kind.len() {
            let name = s.get(t.fields[r][0]).to_owned();
            plan.optimizes.push(read_optimize(&plan, &name, |k| {
                pool.attrs(t.attrs[r])
                    .filter(|(key, _)| s.get(*key) == k)
                    .map(|(_, v)| v.iter().map(|x| s.get(*x)).collect())
                    .collect()
            })?);
        }
        plan.optimizes.sort_by(|a, b| a.name.cmp(&b.name));

        assert!(plan.vars.len() <= TABLE_MAX && plan.corners.len() <= TABLE_MAX);
        assert!(plan.specs.windows(2).all(|w| w[0].output <= w[1].output));
        Ok(plan)
    }

    /// Returns the index of output `name`.
    pub fn output(&self, name: &str) -> Option<u16> {
        let i = self
            .outputs
            .binary_search_by(|o| o.name.as_str().cmp(name))
            .ok()?;
        assert!(i < TABLE_MAX);
        Some(i as u16)
    }

    /// Returns bench var `name`.
    pub fn var(&self, name: &str) -> Option<&Var> {
        self.vars.iter().find(|v| v.name == name)
    }

    /// Returns corner `name`.
    pub fn corner(&self, name: &str) -> Option<&Corner> {
        self.corners.iter().find(|c| c.name == name)
    }
}

fn read_dist(var: &str, d: &[&str]) -> Result<Dist, PlanError> {
    let bad = |why: &str| plan_err("var", var, format!("dist: {why}"));
    let kind = match d.first().copied() {
        Some("gauss") => DistKind::Gauss,
        Some("uniform") => DistKind::Uniform,
        _ => return Err(bad("expected `dist gauss|uniform <spread> [rel]`")),
    };
    let rel = match d.get(2).copied() {
        None => false,
        Some("rel") if d.len() == 3 => true,
        _ => return Err(bad("expected `dist gauss|uniform <spread> [rel]`")),
    };
    let spread = d.get(1).ok_or_else(|| bad("missing spread"))?;
    let prog = Program::compile(spread, Mode::Scalar).map_err(|e| bad(&e.to_string()))?;
    if prog.idents().any(|i| i == var) {
        return Err(bad("the spread cannot reference the var it varies"));
    }
    Ok(Dist {
        var: var.to_owned(),
        kind,
        spread: (*spread).to_owned(),
        rel,
    })
}

fn read_optimize<'a>(
    plan: &Plan,
    name: &str,
    lines: impl Fn(&str) -> Vec<Vec<&'a str>>,
) -> Result<Optimize, PlanError> {
    let bad = |why: String| plan_err("optimize", name, why);
    let one = |k: &str| {
        lines(k)
            .into_iter()
            .next()
            .and_then(|v| v.first().map(|x| (*x).to_owned()))
    };
    let mut goals = Vec::new();
    let mut weights = Vec::new();
    let shape = || bad("goal: expected `<output> min|max|target <expr> [weight <w>]`".into());
    for g in lines("goal") {
        let output = plan
            .output(g[0])
            .ok_or_else(|| bad(format!("goal names no output `{}`", g[0])))?;
        let (goal, rest) = match (g.get(1).copied(), g.get(2)) {
            (Some("min"), _) => (Goal::Min, &g[2..]),
            (Some("max"), _) => (Goal::Max, &g[2..]),
            (Some("target"), Some(t)) => (
                Goal::Target(const_value(t).map_err(|e| bad(format!("target: {e}")))?),
                &g[3..],
            ),
            _ => return Err(shape()),
        };
        let weight = match rest {
            [] => 1.0,
            ["weight", w] => const_value(w).map_err(|e| bad(format!("weight: {e}")))?,
            _ => return Err(shape()),
        };
        if !(weight.is_finite() && weight > 0.0) {
            return Err(bad(format!("weight {weight} is not a positive number")));
        }
        goals.push((output, goal));
        weights.push(weight);
    }
    if goals.is_empty() {
        return Err(bad("has no `goal`".into()));
    }
    let vary: Vec<String> = lines("vary").into_iter().map(|v| v[0].to_owned()).collect();
    if vary.is_empty() {
        return Err(bad("has no `vary`".into()));
    }
    for v in &vary {
        var_bounds(plan, v).map_err(|e| bad(format!("vary {v}: {e}")))?;
    }
    let configs: Vec<String> = lines("config")
        .into_iter()
        .map(|v| v[0].to_owned())
        .collect();
    if let Some(c) = configs.iter().find(|c| !plan.configs.contains(c)) {
        return Err(bad(format!("names no config `{c}`")));
    }
    let corners: Vec<String> = lines("corner")
        .into_iter()
        .map(|v| v[0].to_owned())
        .collect();
    if let Some(c) = corners.iter().find(|c| plan.corner(c).is_none()) {
        return Err(bad(format!("names no corner `{c}`")));
    }
    let budget = one("budget").map_or(Ok(100), |b| parse_count(&b, BUDGET_MAX));
    let seed = one("seed").map_or(Ok(1), |x| {
        x.parse::<u64>().map_err(|_| "not a u64".to_owned())
    });
    let opt = Optimize {
        name: name.to_owned(),
        algo: one("algo").unwrap_or_else(|| "nelder-mead".to_owned()),
        goals,
        weights,
        vary,
        configs,
        corners,
        budget: budget.map_err(|e| bad(format!("budget: {e}")))?,
        seed: seed.map_err(|e| bad(format!("seed: {e}")))?,
    };
    assert!(!opt.goals.is_empty() && !opt.vary.is_empty());
    assert_eq!(opt.goals.len(), opt.weights.len());
    Ok(opt)
}

/// Returns `(min, max, init)` of bench var `name` for the optimizer: both bounds must be
/// constant expressions with `min ≤ max`; `init` is the var's own value when constant,
/// else the midpoint, clamped into the bounds.
pub(crate) fn var_bounds(plan: &Plan, name: &str) -> Result<(f64, f64, f64), String> {
    let v = plan.var(name).ok_or("is not a bench var")?;
    let (Some(lo), Some(hi)) = (&v.min, &v.max) else {
        return Err("needs `min` and `max` bounds (D13: the optimizer clamps to them)".into());
    };
    let (lo, hi) = (const_value(lo)?, const_value(hi)?);
    if lo > hi {
        return Err(format!("min {lo} > max {hi}"));
    }
    let init = const_value(&v.expr)
        .unwrap_or(0.5 * (lo + hi))
        .clamp(lo, hi);
    assert!(lo <= init && init <= hi);
    Ok((lo, hi, init))
}

/// Evaluates a constant scalar expression (`1k`, `2*pi*1meg`) to a number.
///
/// # Errors
///
/// Compile errors, identifiers (`X004`: constants only), and non-numeric results.
pub(crate) fn const_value(src: &str) -> Result<f64, String> {
    let prog = Program::compile(src, Mode::Scalar).map_err(|e| e.to_string())?;
    let mut inputs = Vec::new();
    prog.bind(&mut inputs, |_| None)
        .map_err(|e| e.to_string())?;
    match prog.eval(&inputs, &mut Scratch::default()) {
        Ok(Value::Num(x)) => Ok(x),
        Ok(Value::Int(i)) => Ok(i as f64),
        Ok(other) => Err(format!("`{src}` is not a number ({other:?})")),
        Err(e) => Err(e.to_string()),
    }
}

/// Parses a positive count `1..=max` (format numbers carry no exponent).
pub(crate) fn parse_count(text: &str, max: u32) -> Result<u32, String> {
    match text.parse::<u32>() {
        Ok(n) if (1..=max).contains(&n) => Ok(n),
        _ => Err(format!("`{text}` is not a count in 1..={max}")),
    }
}
