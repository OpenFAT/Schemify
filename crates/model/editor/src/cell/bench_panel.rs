//! The bench sections of the cell panel: analyses, sweeps, corners and optimize records as
//! UI records, each field a node whose id names its target.
//!
//! Contents:
//! - `pub(super) fn bench_sections`

use schemify_doc::{Document, Loc, TableId};

use crate::docs::{attr, node};

use super::panel::{block, button, dropdown, input, note, section};
use super::{ALGOS, Bench, attr_of, joined, record_count};

/// The Outputs, Specs and Optimizers sections of a `.shbench`.
pub(super) fn bench_sections(t: &mut String, doc: &Document) {
    let rows = |b: Bench| record_count(doc, b.table());
    section(t, "cell.s.output", "Outputs (one number per job)");
    for r in 0..rows(Bench::Output) {
        let rec = doc.record(Loc {
            table: TableId::Outputs,
            row: r as u32,
        });
        let blk = block(t, &format!("cell.output.{r}"));
        input(
            t,
            &format!("cell.output.{r}.name"),
            "name",
            &rec.fields[0],
            &blk,
        );
        let expr = rec.fields.get(1).map_or("", String::as_str);
        input(t, &format!("cell.output.{r}.expr"), "expr", expr, &blk);
        input(
            t,
            &format!("cell.output.{r}.unit"),
            "unit",
            attr_of(&rec, "unit").unwrap_or(""),
            &blk,
        );
        button(
            t,
            &format!("cell.output.{r}.del"),
            "Delete output",
            "cell.delete",
            "",
            &blk,
        );
    }
    button(
        t,
        "cell.add.output",
        "+ Output",
        "cell.add_bench",
        "Add an output expression",
        "cell.page",
    );

    section(t, "cell.s.spec", "Specs");
    for r in 0..rows(Bench::Spec) {
        let rec = doc.record(Loc {
            table: TableId::Specs,
            row: r as u32,
        });
        let blk = block(t, &format!("cell.spec.{r}"));
        node(t, &format!("cell.spec.{r}.title"), "label", &blk);
        attr(t, "label", &rec.fields[0]);
        attr(t, "style", "cell-title");
        for a in ["min", "max"] {
            input(
                t,
                &format!("cell.spec.{r}.{a}"),
                a,
                attr_of(&rec, a).unwrap_or(""),
                &blk,
            );
        }
        button(
            t,
            &format!("cell.spec.{r}.del"),
            "Delete spec",
            "cell.delete",
            "",
            &blk,
        );
    }
    button(
        t,
        "cell.add.spec",
        "+ Spec",
        "cell.add_bench",
        "Add a spec for an output without one",
        "cell.page",
    );

    section(t, "cell.s.opt", "Optimizers");
    for r in 0..rows(Bench::Optimize) {
        let rec = doc.record(Loc {
            table: TableId::Optimizes,
            row: r as u32,
        });
        let blk = block(t, &format!("cell.opt.{r}"));
        node(t, &format!("cell.opt.{r}.title"), "label", &blk);
        attr(t, "label", &format!("optimize {}", rec.fields[0]));
        attr(t, "style", "cell-title");
        let algo = attr_of(&rec, "algo").unwrap_or("nelder-mead");
        dropdown(t, &format!("cell.opt.{r}.algo"), "algo", algo, &ALGOS, &blk);
        let goals = joined(&rec, "goal", "; ");
        input(t, &format!("cell.opt.{r}.goals"), "goals", &goals, &blk);
        input(
            t,
            &format!("cell.opt.{r}.vary"),
            "vary",
            &joined(&rec, "vary", " "),
            &blk,
        );
        input(
            t,
            &format!("cell.opt.{r}.corners"),
            "corners",
            &joined(&rec, "corner", " "),
            &blk,
        );
        input(
            t,
            &format!("cell.opt.{r}.configs"),
            "configs",
            &joined(&rec, "config", " "),
            &blk,
        );
        for a in ["budget", "seed"] {
            input(
                t,
                &format!("cell.opt.{r}.{a}"),
                a,
                attr_of(&rec, a).unwrap_or(""),
                &blk,
            );
        }
        note(
            t,
            &format!("cell.opt.{r}.hint"),
            "goals: `<output> min|max|target <expr> [weight <w>]`, separated by `;`. vary: docs variables (their bounds apply).",
        );
        button(
            t,
            &format!("cell.opt.{r}.del"),
            "Delete optimizer",
            "cell.delete",
            "",
            &blk,
        );
    }
    button(
        t,
        "cell.add.opt",
        "+ Optimizer",
        "cell.add_bench",
        "Add an optimizer setup",
        "cell.page",
    );
}
