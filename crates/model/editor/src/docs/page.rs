//! The page as UI records: each block a preview or its source (prose with plots and math,
//! vars with their values and bound status).
//!
//! Contents:
//! - `pub struct Extras`
//! - `pub fn contribute`
//! - `pub(super) fn var_expr`
//! - `pub(super) fn preview`
//! - `pub(super) fn source`
//! - `pub(super) fn prose`
//! - `pub(super) fn var_row`
//! - `pub(super) fn bound_status`
//! - `pub(super) fn eval_in_cell`

use schemify_doc::{Document, RecordKind};
use schemify_elab::{CellState, Val};
use schemify_expr::{Mode, Program, Scratch, Value};
use schemify_ui::prose::{Segment, split_plots};

use super::{Page, attr, editor_id, node, prose_text, value_text, var_value};

/// What the page shows besides its document.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extras<'a> {
    /// `(expression, None when its graph has data | Some(why the slot is empty))` per plot
    /// slot with results; a fence missing here shows "no results yet".
    pub plots: &'a [(String, Option<String>)],
    /// The staged edit under review (D18 optimizer, plugin stages): its label and the
    /// document with it applied (`doc::Project::preview`).
    pub staged: Option<(&'a str, &'a Document)>,
    /// Show prose blocks as their verbatim Markdown / LaTeX source (`docs.source`) instead of
    /// rendered; var blocks and an open editor are unaffected.
    pub source: bool,
}

/// Appends the page of `doc` to the state contribution `t`: one `docs.b.<row>` panel per
/// block under `docs.page`. `cell` supplies var values; `None` (not yet elaborated) shows
/// expressions without values. A plot slot with data becomes a `graph` node, recorded in
/// `graphs` as `(node id, expression)` for the painter; a staged edit adds a banner with
/// Accept / Reject and its new expression beside each var it changes (the docs half of the
/// visual diff).
///
/// Cost: O(blocks) text per call; the shell reparses only when the text changed.
// ponytail: every block is a node (≤ 2^12, the doc capacity), rebuilt on every input frame
// and laid out and painted whenever the page shows. Not windowed like the text tab, because a
// block's height (wrapped prose, math, images, tables, graphs) is known only once `ui` has
// shaped it at the page's width, so spacers for blocks off screen can't be sized without
// paying for exactly the shaping a window would skip. Upgrade path: `ui` keeps the measured
// height of each prose source per width (its prose cache already holds the layout), the page
// windows by those heights and estimates unmeasured blocks, re-anchoring on the first block
// on screen.
pub fn contribute(
    t: &mut String,
    doc: &Document,
    cell: Option<&CellState>,
    page: &Page,
    extras: Extras<'_>,
    graphs: &mut Vec<(String, String)>,
) {
    let blocks = doc.doc_blocks();
    let s = doc.strings();
    let n = blocks.kind.len();
    assert!(n <= 1 << 12, "doc blocks over capacity");
    if let Some((label, _)) = extras.staged {
        node(t, "docs.staged", "panel", "docs.page");
        attr(t, "style", "docs-staged-bar");
        node(t, "docs.staged.label", "label", "docs.staged");
        attr(t, "label", &format!("Staged: {label}"));
        attr(t, "style", "docs-staged");
        for (id, text, cmd) in [
            ("docs.staged.accept", "Accept", "edit.accept_staged"),
            ("docs.staged.reject", "Reject", "edit.reject_staged"),
        ] {
            node(t, id, "button", "docs.staged");
            attr(t, "label", text);
            attr(t, "command", cmd);
        }
    }
    if n == 0 {
        node(t, "docs.empty", "label", "docs.page");
        attr(
            t,
            "label",
            "No docs yet: + Text adds a text block, + Var a variable.",
        );
        attr(t, "style", "docs-hint");
    }
    let mut v = 0usize;
    for r in 0..n as u32 {
        let id = format!("docs.b.{r}");
        let style = if page.sel == Some(r) {
            "docs-block-sel"
        } else {
            "docs-block"
        };
        node(t, &id, "panel", "docs.page");
        attr(t, "style", style);
        match blocks.kind[r as usize] {
            RecordKind::Prose => match &page.editing {
                Some((e, text)) if *e == r => {
                    node(t, &editor_id(r), "text_area", &id);
                    attr(t, "value", text);
                    attr(t, "command", "docs.prose_ok");
                    attr(t, "input", "field.typed");
                    preview(t, &id, text);
                    node(t, &format!("{id}.hint"), "label", &id);
                    attr(
                        t,
                        "label",
                        "Markdown with $math$ and $$display math$$; Ctrl+Enter applies, Esc cancels",
                    );
                    attr(t, "style", "docs-hint");
                }
                _ if extras.source => source(t, &id, &prose_text(doc, r)),
                _ => prose(t, &id, &prose_text(doc, r), extras.plots, graphs),
            },
            RecordKind::Var => {
                let f = blocks.fields[r as usize];
                let (name, expr) = (s.get(f[0]), s.get(f[1]));
                let a = |k: &str| {
                    doc.pool()
                        .find_attr(s, blocks.attrs[r as usize], k)
                        .and_then(|vals| vals.first())
                        .map(|v| s.get(*v))
                };
                var_row(
                    t,
                    &id,
                    name,
                    expr,
                    (a("unit"), a("min"), a("max"), a("doc")),
                    cell,
                    v,
                );
                if let Some((_, preview)) = extras.staged
                    && let Some(new) = var_expr(preview, name).filter(|e| *e != expr)
                {
                    node(t, &format!("{id}.staged"), "label", &format!("{id}.row"));
                    attr(t, "label", &format!("→ {new}"));
                    attr(t, "style", "docs-staged");
                }
                v += 1;
            }
            _ => {}
        }
    }
}

/// The expression of var `name` in `doc`, if it has one.
pub(super) fn var_expr<'d>(doc: &'d Document, name: &str) -> Option<&'d str> {
    let b = doc.doc_blocks();
    let s = doc.strings();
    (0..b.kind.len())
        .find(|&r| b.kind[r] == RecordKind::Var && s.get(b.fields[r][0]) == name)
        .map(|r| s.get(b.fields[r][1]))
}

/// The live preview under an open prose editor: a caption and `text` (what the user has
/// typed so far) through the same prose renderer, math included. It carries no command, so a
/// click on it is a click away from the editor (which applies). A ```` ```plot ```` fence
/// previews as a code block; images not yet on the page preview as their alt text.
pub(super) fn preview(t: &mut String, id: &str, text: &str) {
    debug_assert!(id.starts_with("docs.b."));
    node(t, &format!("{id}.preview.cap"), "label", id);
    attr(t, "label", "Preview");
    attr(t, "style", "docs-preview-cap");
    let pid = format!("{id}.preview");
    node(t, &pid, "prose", id);
    let shown = text.trim_end();
    attr(
        t,
        "value",
        if shown.trim().is_empty() {
            "*(empty: applying deletes this block)*"
        } else {
            shown
        },
    );
    attr(t, "style", "docs-preview");
    debug_assert!(
        t.ends_with("\"docs-preview\"\n"),
        "the preview is the last node written"
    );
}

/// A prose block shown as its source (`docs.source`): one prose node holding `text` inside a
/// code fence, so it lays out verbatim in monospace on a faint shade (no markup, no math).
/// Clicking it opens the editor like a rendered block.
pub(super) fn source(t: &mut String, id: &str, text: &str) {
    debug_assert!(id.starts_with("docs.b."));
    let pid = format!("{id}.0");
    node(t, &pid, "prose", id);
    attr(t, "command", "docs.edit");
    attr(t, "style", "docs-source");
    let text = text.trim_end();
    if text.trim().is_empty() {
        attr(t, "value", "*(empty text block: click to edit)*");
        return;
    }
    // A fence the text does not use, so the code block cannot end early.
    // ponytail: a block using both ``` and ~~~ fences shows the rest after its first ~~~ line
    // rendered; a fence of more backticks than the text's longest run lifts that ceiling.
    let fenced = |f: &str| text.lines().any(|l| l.trim_start().starts_with(f));
    let fence = if fenced("~~~") { "```" } else { "~~~" };
    let value = format!("{fence}\n{text}\n{fence}");
    debug_assert!(value.starts_with(fence) && value.ends_with(fence));
    attr(t, "value", &value);
}

/// A prose block: prose segments and `plot` slots in source order. A slot whose expression
/// has data in `plots` is a `graph` node (recorded in `graphs`); the others say why empty.
pub(super) fn prose(
    t: &mut String,
    id: &str,
    text: &str,
    plots: &[(String, Option<String>)],
    graphs: &mut Vec<(String, String)>,
) {
    let parts = split_plots(text);
    if parts.is_empty() {
        node(t, &format!("{id}.0"), "prose", id);
        attr(t, "value", "*(empty text block: click to edit)*");
        attr(t, "command", "docs.edit");
    }
    for (k, part) in parts.into_iter().enumerate() {
        let pid = format!("{id}.{k}");
        match part {
            Segment::Prose(src) => {
                node(t, &pid, "prose", id);
                attr(t, "value", src.trim_end());
                attr(t, "command", "docs.edit");
            }
            Segment::Plot(expr) => {
                let expr = expr.trim();
                let state = plots.iter().find(|p| p.0 == expr).map(|p| p.1.as_deref());
                if let Some(None) = state {
                    node(t, &pid, "graph", id);
                    attr(t, "style", "docs-graph");
                    graphs.push((pid, expr.to_owned()));
                    continue;
                }
                node(t, &pid, "panel", id);
                attr(t, "style", "docs-plot");
                node(t, &format!("{pid}.expr"), "label", &pid);
                attr(t, "label", &format!("plot  {expr}"));
                node(t, &format!("{pid}.none"), "label", &pid);
                let why = state
                    .flatten()
                    .unwrap_or("no results yet: run the bench to fill this graph");
                attr(t, "label", why);
                attr(t, "style", "docs-hint");
            }
        }
    }
}

/// A var row: `name = [expr] = value unit` plus bound and D20 markers.
pub(super) fn var_row(
    t: &mut String,
    id: &str,
    name: &str,
    expr: &str,
    (unit, min, max, help): (Option<&str>, Option<&str>, Option<&str>, Option<&str>),
    cell: Option<&CellState>,
    v: usize,
) {
    let row = format!("{id}.row");
    node(t, &row, "panel", id);
    attr(t, "style", "docs-var");
    node(t, &format!("{id}.name"), "label", &row);
    attr(t, "label", name);
    attr(t, "command", "docs.select");
    attr(t, "style", "docs-var-name");
    if let Some(h) = help {
        attr(t, "tooltip", h);
    }
    node(t, &format!("{id}.eq"), "label", &row);
    attr(t, "label", "=");
    node(t, &format!("{id}.expr"), "text_input", &row);
    attr(t, "value", expr);
    attr(t, "command", "docs.var_ok");
    attr(t, "style", "docs-expr");
    let Some((val, unverified)) = cell.and_then(|c| var_value(c, name, v)) else {
        return;
    };
    let (text, ok) = match val {
        Val::Err(e) => (format!("{:?} {}", e.code, e.msg), false),
        Val::Upstream => ("(an input has an error)".to_owned(), false),
        _ => {
            let shown = value_text(val, unit.unwrap_or(""));
            match bound_status(val, min, max, cell.expect("value implies a cell")) {
                Some(why) => (format!("= {shown}  ({why})"), false),
                None => (format!("= {shown}"), true),
            }
        }
    };
    node(t, &format!("{id}.val"), "label", &row);
    attr(t, "label", &text);
    attr(t, "style", if ok { "docs-value" } else { "docs-value-bad" });
    if unverified {
        node(t, &format!("{id}.unverified"), "label", &row);
        attr(t, "label", "unverified (cached; plugin not loaded)");
        attr(t, "style", "docs-unverified");
        attr(
            t,
            "tooltip",
            "D20: the plugin this value needs is missing; the value stored in the file is shown",
        );
    }
}

/// D13 status of a var against its `min`/`max` (expressions in the cell scope): `None` when
/// inside, else why not. A bound that does not evaluate is reported as such.
pub(super) fn bound_status(
    val: &Val,
    min: Option<&str>,
    max: Option<&str>,
    cell: &CellState,
) -> Option<String> {
    let x = val.as_f64()?;
    for (bound, is_min) in [(min, true), (max, false)] {
        let Some(src) = bound else { continue };
        let Some(b) = eval_in_cell(src, cell) else {
            return Some(format!("bound `{src}` does not evaluate"));
        };
        if (is_min && x < b) || (!is_min && x > b) {
            let which = if is_min { "below min" } else { "above max" };
            return Some(format!("{which} {}", schemify_wave::format_si(b, 4)));
        }
    }
    None
}

/// Evaluates a scalar expression over the cell's own params and vars (current values).
pub(super) fn eval_in_cell(src: &str, cell: &CellState) -> Option<f64> {
    let prog = Program::compile(src, Mode::Scalar).ok()?;
    let g = &cell.graph;
    let own = cell.ifaces.first()?;
    let mut inputs = Vec::new();
    prog.bind(&mut inputs, |name| {
        let n = cell.names.find(name)?;
        let node = own.param_name.iter().position(|&p| p == n).or_else(|| {
            g.var_name
                .iter()
                .position(|&x| x == n)
                .map(|v| g.params as usize + v)
        })?;
        g.vals.get(node)?.value()
    })
    .ok()?;
    match prog.eval(&inputs, &mut Scratch::default()).ok()? {
        Value::Num(x) => Some(x),
        Value::Int(i) => Some(i as f64),
        _ => None,
    }
}
