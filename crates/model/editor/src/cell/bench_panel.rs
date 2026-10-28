//! The bench sections of the cell panel: analyses, sweeps, corners and optimize records as
//! UI records, each field a node whose id names its target.
//!
//! Contents:
//! - `pub(super) fn bench_sections`

use schemify_doc::{Document, Loc, TableId};

use crate::docs;

use super::panel;
use super::{ALGOS, Bench};

/// The Outputs, Specs and Optimizers sections of a `.shbench`.
pub(super) fn bench_sections(t: &mut String, doc: &Document) {
    assert!(
        !t.is_empty(),
        "the bench sections follow the panel's header"
    );
    let start = t.len();
    let rows = |b: Bench| super::record_count(doc, b.table());
    panel::section(t, "cell.s.output", "Outputs (one number per job)");
    for r in 0..rows(Bench::Output) {
        let rec = doc.record(Loc {
            table: TableId::Outputs,
            row: super::row_u32(r),
        });
        let blk = panel::block(t, &format!("cell.output.{r}"));
        panel::input(
            t,
            &format!("cell.output.{r}.name"),
            "name",
            &rec.fields[0],
            &blk,
        );
        let expr = rec.fields.get(1).map_or("", String::as_str);
        panel::input(t, &format!("cell.output.{r}.expr"), "expr", expr, &blk);
        panel::input(
            t,
            &format!("cell.output.{r}.unit"),
            "unit",
            super::attr_of(&rec, "unit").unwrap_or(""),
            &blk,
        );
        panel::button(
            t,
            &format!("cell.output.{r}.del"),
            "Delete output",
            "cell.delete",
            "",
            &blk,
        );
    }
    panel::button(
        t,
        "cell.add.output",
        "+ Output",
        "cell.add_bench",
        "Add an output expression",
        "cell.page",
    );

    panel::section(t, "cell.s.spec", "Specs");
    for r in 0..rows(Bench::Spec) {
        let rec = doc.record(Loc {
            table: TableId::Specs,
            row: super::row_u32(r),
        });
        let blk = panel::block(t, &format!("cell.spec.{r}"));
        docs::node(t, &format!("cell.spec.{r}.title"), "label", &blk);
        docs::attr(t, "label", &rec.fields[0]);
        docs::attr(t, "style", "cell-title");
        for a in ["min", "max"] {
            panel::input(
                t,
                &format!("cell.spec.{r}.{a}"),
                a,
                super::attr_of(&rec, a).unwrap_or(""),
                &blk,
            );
        }
        panel::button(
            t,
            &format!("cell.spec.{r}.del"),
            "Delete spec",
            "cell.delete",
            "",
            &blk,
        );
    }
    panel::button(
        t,
        "cell.add.spec",
        "+ Spec",
        "cell.add_bench",
        "Add a spec for an output without one",
        "cell.page",
    );

    panel::section(t, "cell.s.opt", "Optimizers");
    for r in 0..rows(Bench::Optimize) {
        let rec = doc.record(Loc {
            table: TableId::Optimizes,
            row: super::row_u32(r),
        });
        let blk = panel::block(t, &format!("cell.opt.{r}"));
        docs::node(t, &format!("cell.opt.{r}.title"), "label", &blk);
        docs::attr(t, "label", &format!("optimize {}", rec.fields[0]));
        docs::attr(t, "style", "cell-title");
        let algo = super::attr_of(&rec, "algo").unwrap_or("nelder-mead");
        panel::dropdown(t, &format!("cell.opt.{r}.algo"), "algo", algo, &ALGOS, &blk);
        let goals = super::joined(&rec, "goal", "; ");
        panel::input(t, &format!("cell.opt.{r}.goals"), "goals", &goals, &blk);
        panel::input(
            t,
            &format!("cell.opt.{r}.vary"),
            "vary",
            &super::joined(&rec, "vary", " "),
            &blk,
        );
        panel::input(
            t,
            &format!("cell.opt.{r}.corners"),
            "corners",
            &super::joined(&rec, "corner", " "),
            &blk,
        );
        panel::input(
            t,
            &format!("cell.opt.{r}.configs"),
            "configs",
            &super::joined(&rec, "config", " "),
            &blk,
        );
        for a in ["budget", "seed"] {
            panel::input(
                t,
                &format!("cell.opt.{r}.{a}"),
                a,
                super::attr_of(&rec, a).unwrap_or(""),
                &blk,
            );
        }
        panel::note(
            t,
            &format!("cell.opt.{r}.hint"),
            "goals: `<output> min|max|target <expr> [weight <w>]`, separated by `;`. vary: docs variables (their bounds apply).",
        );
        panel::button(
            t,
            &format!("cell.opt.{r}.del"),
            "Delete optimizer",
            "cell.delete",
            "",
            &blk,
        );
    }
    panel::button(
        t,
        "cell.add.opt",
        "+ Optimizer",
        "cell.add_bench",
        "Add an optimizer setup",
        "cell.page",
    );
    assert!(
        t.len() > start,
        "three sections and their add buttons were written"
    );
}
