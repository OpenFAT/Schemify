//! Bench tables as a run reads them (format spec §4 ranks 4 and 6, run spec §1): one bench
//! [`Document`] in, one checked [`Plan`] out (vars with bounds and Monte Carlo
//! distributions, corners, configs, sweeps, Monte Carlo and optimize records, outputs and
//! specs).
//!
//! Data: every table holds at most [`TABLE_MAX`] rows (doc's bench capacity), read
//! together by name (sweep name → var + values), so these are small AoS rows, not hot
//! columns. Lifetime: one Run; read once, then shared read-only by expansion, results and
//! the optimizer.
//!
//! Contents:
//! - `pub const TABLE_MAX: usize`, `pub const SWEEP_POINTS_MAX: u32`,
//!   `pub const SAMPLES_MAX: u32`, `pub const BUDGET_MAX: u32`
//! - `pub struct PlanError`, `pub struct Var`, `pub struct Corner`, `pub struct Sweep`,
//!   `pub struct MonteCarlo`, `pub enum DistKind`, `pub struct Dist`, `pub enum Goal`
//!   (`Goal::err`), `pub struct Optimize`, `pub struct Output`, `pub struct Spec`
//! - `pub struct Plan`: `Plan::read`, `Plan::output`, `Plan::var`, `Plan::corner`
//! - crate-only: `plan_err`, `sort_by_name`, `var_bounds`, `const_value`, `parse_count`

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
/// lacks. `record` is `<kind> <name>` (`sweep rsw`, `select jobs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanError {
    /// Offending record.
    pub record: String,
    /// What is wrong with it.
    pub detail: String,
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
    /// `sweep` feature, always empty without it.
    pub values: Vec<String>,
}

/// One `montecarlo` record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonteCarlo {
    /// Name.
    pub name: String,
    /// Samples, `1..=SAMPLES_MAX`.
    pub samples: u32,
    /// Seed of the sample streams (0 when absent).
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
    /// Spread expression text (bench scope); never references `var` itself.
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

/// One `optimize` record, checked against the plan's outputs, vars, configs and corners.
#[derive(Clone, Debug, PartialEq)]
pub struct Optimize {
    /// Name.
    pub name: String,
    /// `algo` word (`nelder-mead` when absent; checked by the optimizer).
    pub algo: String,
    /// `(output index, goal)` per `goal` line, file order; never empty.
    pub goals: Vec<(u16, Goal)>,
    /// Score weight of each goal (`weight <w>`, 1 when absent), parallel to `goals`.
    pub weights: Vec<f64>,
    /// Varied vars, file order, never empty; each has constant `min` and `max` bounds.
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
    /// `src` compiled in [`Mode::Calc`].
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

/// Everything a run needs from one bench. Named tables are sorted by name (bytewise),
/// specs by output; vars keep document order.
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
    /// Specs, sorted by output index.
    pub specs: Vec<Spec>,
    /// The bench's `corner` header attribute.
    pub default_corner: Option<String>,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.record, self.detail)
    }
}

impl std::error::Error for PlanError {}

impl Goal {
    /// Returns the error of `value` under this goal, lower is better: `value` (min),
    /// `−value` (max) or `|value − target|` (target). The optimizer scores the weighted
    /// sum and, over one candidate's jobs, keeps each goal's largest.
    pub fn err(self, value: f64) -> f64 {
        match self {
            Goal::Min => value,
            Goal::Max => -value,
            Goal::Target(t) => (value - t).abs(),
        }
    }
}

impl Plan {
    /// Reads the bench tables of `doc` and checks them (run spec §1): sweep shapes and
    /// point counts, `dist` attributes, Monte Carlo sample counts, optimize goals (outputs
    /// exist) and vary vars (constant bounds), output and spec expressions compile.
    ///
    /// Cost: one compile per expression and per sweep or Monte Carlo record.
    ///
    /// # Errors
    ///
    /// The first record that cannot run as written.
    pub fn read(doc: &Document) -> Result<Plan, PlanError> {
        let s = doc.strings();
        let pool = doc.pool();
        let values = |span: Span, key: &str| -> Option<Vec<&str>> {
            pool.find_attr(s, span, key)
                .map(|v| v.iter().map(|x| s.get(*x)).collect())
        };
        let word = |span: Span, key: &str| -> Option<String> {
            values(span, key).and_then(|v| v.first().map(|x| (*x).to_owned()))
        };
        let mut plan = Plan {
            default_corner: doc.header_value("corner").map(str::to_owned),
            ..Plan::default()
        };

        // Vars (rank 4, document order) and their `dist`.
        let t = doc.doc_blocks();
        for r in (0..t.kind.len()).filter(|&r| t.kind[r] == RecordKind::Var) {
            let name = s.get(t.fields[r][0]).to_owned();
            if let Some(d) = values(t.attrs[r], "dist") {
                plan.dists.push(read_dist(&name, &d)?);
            }
            plan.vars.push(Var {
                expr: s.get(t.fields[r][1]).to_owned(),
                min: word(t.attrs[r], "min"),
                max: word(t.attrs[r], "max"),
                name,
            });
        }
        sort_by_name(&mut plan.dists, |d| &d.var);

        // Corners and configs.
        let t = doc.corners();
        for r in 0..t.kind.len() {
            let sets = pool
                .attrs(t.attrs[r])
                .filter(|(k, v)| s.get(*k) == "set" && v.len() == 2)
                .map(|(_, v)| (s.get(v[0]).to_owned(), s.get(v[1]).to_owned()))
                .collect();
            let name = s.get(t.fields[r][0]).to_owned();
            plan.corners.push(Corner { name, sets });
        }
        sort_by_name(&mut plan.corners, |x| &x.name);
        let t = doc.configs();
        plan.configs = (0..t.kind.len())
            .map(|r| s.get(t.fields[r][0]).to_owned())
            .collect();
        plan.configs.sort();

        // Sweeps.
        let t = doc.sweeps();
        for r in 0..t.kind.len() {
            let name = s.get(t.fields[r][0]).to_owned();
            let var =
                word(t.attrs[r], "var").ok_or_else(|| plan_err("sweep", &name, "has no `var`"))?;
            let shapes = ["values", "lin", "log"].map(|k| values(t.attrs[r], k));
            #[cfg(feature = "sweep")]
            let values = crate::sweep::points(&shapes).map_err(|e| plan_err("sweep", &name, e))?;
            #[cfg(not(feature = "sweep"))]
            let values = {
                let _ = shapes;
                Vec::new()
            };
            plan.sweeps.push(Sweep { name, var, values });
        }
        sort_by_name(&mut plan.sweeps, |x| &x.name);

        // Monte Carlo records.
        let t = doc.montecarlos();
        for r in 0..t.kind.len() {
            let name = s.get(t.fields[r][0]).to_owned();
            let bad = |why: String| plan_err("montecarlo", &name, why);
            let samples =
                word(t.attrs[r], "samples").ok_or_else(|| bad("has no `samples`".into()))?;
            let samples =
                parse_count(&samples, SAMPLES_MAX).map_err(|e| bad(format!("samples: {e}")))?;
            let seed = word(t.attrs[r], "seed").map_or(Ok(0), |x| x.parse::<u64>());
            let seed = seed.map_err(|_| bad("seed is not a u64".into()))?;
            plan.montecarlos.push(MonteCarlo {
                name,
                samples,
                seed,
            });
        }
        sort_by_name(&mut plan.montecarlos, |x| &x.name);

        // Outputs and specs (before optimize: goals name outputs).
        let t = doc.outputs();
        for r in 0..t.kind.len() {
            let name = s.get(t.fields[r][0]).to_owned();
            let src = s.get(t.fields[r][1]).to_owned();
            let prog = Program::compile(&src, Mode::Calc)
                .map_err(|e| plan_err("output", &name, e.to_string()))?;
            plan.outputs.push(Output { name, src, prog });
        }
        sort_by_name(&mut plan.outputs, |x| &x.name);
        assert!(plan.outputs.len() <= TABLE_MAX, "doc caps bench tables");
        let t = doc.specs();
        for r in 0..t.kind.len() {
            let name = s.get(t.fields[r][0]);
            let output = plan
                .output(name)
                .ok_or_else(|| plan_err("spec", name, "names no output"))?;
            let bound = |k| -> Result<Option<Program>, PlanError> {
                let src = values(t.attrs[r], k).and_then(|v| v.first().copied());
                let prog = src
                    .map(|src| Program::compile(src, Mode::Scalar))
                    .transpose();
                prog.map_err(|e| plan_err("spec", name, format!("{k}: {e}")))
            };
            plan.specs.push(Spec {
                output,
                min: bound("min")?,
                max: bound("max")?,
            });
        }
        plan.specs.sort_by_key(|sp| sp.output);

        // Optimize records.
        let t = doc.optimizes();
        for r in 0..t.kind.len() {
            let name = s.get(t.fields[r][0]).to_owned();
            let lines = |k: &str| -> Vec<Vec<&str>> {
                let rows = pool.attrs(t.attrs[r]).filter(|(key, _)| s.get(*key) == k);
                rows.map(|(_, v)| v.iter().map(|x| s.get(*x)).collect())
                    .collect()
            };
            plan.optimizes.push(read_optimize(&plan, &name, lines)?);
        }
        sort_by_name(&mut plan.optimizes, |x| &x.name);

        assert!(plan.vars.len() <= TABLE_MAX);
        assert!(plan.corners.len() <= TABLE_MAX);
        assert!(plan.specs.windows(2).all(|w| w[0].output <= w[1].output));
        Ok(plan)
    }

    /// Returns the index of output `name` (binary search: outputs are sorted).
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

/// Builds the error of record `<kind> <name>`.
pub(crate) fn plan_err(kind: &str, name: &str, detail: impl Into<String>) -> PlanError {
    PlanError {
        record: format!("{kind} {name}"),
        detail: detail.into(),
    }
}

/// Sorts `v` stably by `name`: one `(name, position)` sort shared by every row type,
/// instead of one merge sort instantiated per type and closure (binary size, D39).
pub(crate) fn sort_by_name<T>(v: &mut Vec<T>, name: impl Fn(&T) -> &String) {
    let mut keyed: Vec<(&str, u32)> = v
        .iter()
        .zip(0..)
        .map(|(x, i)| (name(x).as_str(), i))
        .collect();
    keyed.sort_unstable();
    let order: Vec<u32> = keyed.into_iter().map(|k| k.1).collect();
    let mut old: Vec<Option<T>> = v.drain(..).map(Some).collect();
    v.extend(
        order
            .iter()
            .map(|&i| old[i as usize].take().expect("a permutation")),
    );
    assert_eq!(v.len(), old.len(), "a permutation");
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
    assert!(lo <= init);
    assert!(init <= hi);
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

// ---- private ----

fn read_dist(var: &str, d: &[&str]) -> Result<Dist, PlanError> {
    let bad = |why: &str| plan_err("var", var, format!("dist: {why}"));
    let shape = "expected `dist gauss|uniform <spread> [rel]`";
    let kind = match d.first().copied() {
        Some("gauss") => DistKind::Gauss,
        Some("uniform") => DistKind::Uniform,
        _ => return Err(bad(shape)),
    };
    let rel = match d.get(2).copied() {
        None => false,
        Some("rel") if d.len() == 3 => true,
        _ => return Err(bad(shape)),
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

/// Reads one `optimize` record whose attribute lines `lines(key)` returns, in file order.
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
    let firsts =
        |k: &str| -> Vec<String> { lines(k).into_iter().map(|v| v[0].to_owned()).collect() };

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
            (Some("target"), Some(t)) => {
                let t = const_value(t).map_err(|e| bad(format!("target: {e}")))?;
                (Goal::Target(t), &g[3..])
            }
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
    let vary = firsts("vary");
    if vary.is_empty() {
        return Err(bad("has no `vary`".into()));
    }
    for v in &vary {
        var_bounds(plan, v).map_err(|e| bad(format!("vary {v}: {e}")))?;
    }
    let configs = firsts("config");
    if let Some(c) = configs.iter().find(|c| !plan.configs.contains(c)) {
        return Err(bad(format!("names no config `{c}`")));
    }
    let corners = firsts("corner");
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
    assert!(!opt.goals.is_empty());
    assert!(!opt.vary.is_empty());
    assert_eq!(opt.goals.len(), opt.weights.len());
    Ok(opt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_by_name_is_stable() {
        let mut v: Vec<(String, u8)> = [("b", 0), ("a", 1), ("b", 2), ("a", 3)]
            .map(|(n, k)| (n.to_owned(), k))
            .to_vec();
        sort_by_name(&mut v, |x| &x.0);
        let keys: Vec<u8> = v.iter().map(|x| x.1).collect();
        assert_eq!(keys, [1, 3, 0, 2]);
    }

    #[test]
    fn constants_and_counts() {
        assert_eq!(const_value("2*1k"), Ok(2000.0));
        assert!(const_value("R").is_err(), "constants only");
        assert!(const_value("1 +").is_err());
        assert_eq!(parse_count("3", 3), Ok(3));
        assert!(parse_count("0", 3).is_err());
        assert!(parse_count("4", 3).is_err());
        assert!(parse_count("1e2", 300).is_err(), "no exponent");
    }

    #[test]
    fn goal_errors_rank_lower_as_better() {
        assert_eq!(Goal::Min.err(2.0), 2.0);
        assert_eq!(Goal::Max.err(2.0), -2.0);
        assert_eq!(Goal::Target(3.0).err(1.0), 2.0);
        assert_eq!(Goal::Target(3.0).err(5.0), 2.0);
    }
}
