//! `docs`: the docs page of the active level (D19, D21, `docs/spec/docs-page.md`): its doc
//! blocks projected as UI nodes into the plugin-declared `docs.page` scroll pane, beside the
//! canvas, and the `doc` edit transactions the page makes (one per edit, so one undo). Also
//! the D20 cache write-back: plugin-computed var values copied into `cached` attributes.
//!
//! Data design:
//! 1. In: one document's `doc_blocks` rows and its [`CellState`] values. Out: state
//!    contribution records (UI nodes) and `doc` ops.
//! 2. How many: ≤ 2^12 blocks per cell (doc capacity); a page shows them all.
//! 3. Widths: block rows `u32` (< 2^12); var values are elab's [`Val`]s.
//! 4. Access: every block once per frame while the page is shown — the contribution text is
//!    compared and reparsed only when it changed (`Shell::set_state`).
//! 5. Lifetime: [`Page`] lives with the editor and resets when the active document changes.
//! 6. Parallel: no (one writer, a few hundred rows).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, Document, Loc, NEW_ID, Op, RecordBuf, RecordKind, TableId, Target};
use schemify_elab::{CellState, Val};
use schemify_expr::{Mode, Program, Scratch, Value};
use schemify_ui::prose::{Segment, split_plots};
use schemify_ui::{Image, TextSystem};

use crate::platform;

/// The page's interaction state (owner: the editor).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    /// Document the state belongs to; another active document resets it.
    pub doc: Option<DocHandle>,
    /// Selected block row: target of insert (after it), move and delete.
    pub sel: Option<u32>,
    /// Prose block open in the text editor: its row and the text typed so far.
    pub editing: Option<(u32, String)>,
    /// The editor field has had keyboard focus (so losing it means "done").
    pub focused: bool,
    /// Prose image paths that failed to load (they show their alt text); retried when the
    /// page is shown for another document and back. At most [`IMAGES_FAILED_MAX`].
    pub images_failed: Vec<String>,
}

/// Failed image paths a page remembers; past it, the oldest is retried.
pub const IMAGES_FAILED_MAX: usize = 64;
/// Largest image file read for the docs page.
pub const IMAGE_BYTES_MAX: usize = 16 << 20;
/// Longest image side, in pixels.
pub const IMAGE_SIDE_MAX: u32 = 4096;

impl Page {
    /// Resets the state when the shown document changes.
    pub fn follow(&mut self, doc: DocHandle) {
        if self.doc != Some(doc) {
            *self = Page {
                doc: Some(doc),
                ..Page::default()
            };
        }
        debug_assert_eq!(self.doc, Some(doc));
    }
}

/// Block row named by a page node id (`docs.b.<row>…`).
pub fn row_of(node: &str) -> Option<u32> {
    let rest = node.strip_prefix("docs.b.")?;
    rest.split('.').next()?.parse().ok()
}

/// Node id of the prose editor of block `row`.
pub fn editor_id(row: u32) -> String {
    format!("docs.b.{row}.edit")
}

/// The verbatim text of prose block `row` (lines joined by `\n`).
pub fn prose_text(doc: &Document, row: u32) -> String {
    let rec = doc.record(Loc {
        table: TableId::DocBlocks,
        row,
    });
    debug_assert_eq!(rec.kind, RecordKind::Prose);
    rec.verbatim.join("\n")
}

/// Kind of block `row`, if it exists.
pub fn kind_of(doc: &Document, row: u32) -> Option<RecordKind> {
    doc.doc_blocks().kind.get(row as usize).copied()
}

/// The calculator expressions of every ```` ```plot ```` fence of `doc`'s prose blocks, in
/// page order (the slots the run fills, `docs-page.md` §5).
pub fn plot_exprs(doc: &Document) -> Vec<String> {
    let b = doc.doc_blocks();
    let mut out = Vec::new();
    for r in 0..b.kind.len() as u32 {
        if b.kind[r as usize] != RecordKind::Prose {
            continue;
        }
        for part in split_plots(&prose_text(doc, r)) {
            if let Segment::Plot(e) = part {
                out.push(e.trim().to_owned());
            }
        }
    }
    out
}

/// Registers in `text` every image the prose blocks of `doc` show (`![alt](path)` alone on a
/// line, [`schemify_ui::prose::image_line`]) that is not registered yet and not in `failed`:
/// [`load_image`] from the project at `root`. A path that fails joins `failed` (it shows its
/// alt text). Returns whether one loaded (the page needs a new layout) and the last failure.
///
/// Cost: one pass over the prose lines; file reads only for new paths.
pub fn load_images(
    doc: &Document,
    root: &Path,
    text: &mut TextSystem,
    failed: &mut Vec<String>,
) -> (bool, Option<String>) {
    let b = doc.doc_blocks();
    let (mut loaded, mut err) = (false, None);
    for r in 0..b.kind.len() {
        if b.kind[r] != RecordKind::Prose {
            continue;
        }
        let rec = doc.record(Loc {
            table: TableId::DocBlocks,
            row: r as u32,
        });
        for line in &rec.verbatim {
            let Some((_, path)) = schemify_ui::prose::image_line(line.trim()) else {
                continue;
            };
            if text.image_id(path).is_some() || failed.iter().any(|f| f == path) {
                continue;
            }
            match load_image(root, path) {
                Ok(img) => {
                    loaded |= text.set_image(img);
                    if text.image_id(path).is_none() {
                        err = Some(format!("{path}: too many images on the page"));
                    }
                }
                Err(e) => err = Some(e),
            }
            if text.image_id(path).is_none() {
                if failed.len() >= IMAGES_FAILED_MAX {
                    failed.remove(0);
                }
                failed.push(path.to_owned());
            }
        }
    }
    debug_assert!(failed.len() <= IMAGES_FAILED_MAX);
    (loaded, err)
}

/// The file of image `path` in the project at `root`: `path` must be project-relative with `/`
/// separators, without an empty, `.` or `..` segment, `\`, `:` or NUL (the plugin ABI's path
/// rule, plugin-abi §5), so prose cannot name a file outside the project.
///
/// # Errors
///
/// Any other path, with the reason.
pub fn image_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    let inside = path.len() <= 4096
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..");
    if !inside {
        return Err(format!("{path}: images must be inside the project"));
    }
    let file = root.join(path);
    debug_assert!(file.starts_with(root));
    Ok(file)
}

/// Reads and decodes PNG image `path` of the project at `root` (premultiplied RGBA8).
///
/// # Errors
///
/// A path outside the project ([`image_path`]), an unreadable file, more than
/// [`IMAGE_BYTES_MAX`] bytes, not a PNG, a side of 0 or over [`IMAGE_SIDE_MAX`] (checked in the
/// header, before anything is allocated for the pixels), or a decoding error.
pub fn load_image(root: &Path, path: &str) -> Result<Image, String> {
    let bytes = platform::read(&image_path(root, path)?)?;
    if bytes.len() > IMAGE_BYTES_MAX {
        return Err(format!("{path}: over {} MiB", IMAGE_BYTES_MAX >> 20));
    }
    let (w, h) = png_size(&bytes).ok_or_else(|| format!("{path}: not a PNG image"))?;
    if w == 0 || h == 0 || w > IMAGE_SIDE_MAX || h > IMAGE_SIDE_MAX {
        return Err(format!(
            "{path}: {w}×{h} px (at most {IMAGE_SIDE_MAX} a side)"
        ));
    }
    let px = schemify_render::Pixmap::from_png(std::io::Cursor::new(&bytes))
        .map_err(|e| format!("{path}: {e}"))?;
    let (pw, ph) = (u32::from(px.width()), u32::from(px.height()));
    assert_eq!((pw, ph), (w, h), "the decoder read the header's size");
    Ok(Image::new(path, w, h, px.data_as_u8_slice().to_vec()))
}

/// Width and height from a PNG's signature and IHDR chunk; `None` when it is not one.
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if b.len() < 24 || b[..8] != SIGNATURE || b[12..16] != *b"IHDR" {
        return None;
    }
    let be = |at: usize| u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    Some((be(16), be(20)))
}

/// What the page shows besides its document.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extras<'a> {
    /// `(expression, None when its graph has data | Some(why the slot is empty))` per plot
    /// slot with results; a fence missing here shows "no results yet".
    pub plots: &'a [(String, Option<String>)],
    /// The staged edit under review (D18 optimizer, plugin stages): its label and the
    /// document with it applied (`doc::Project::preview`).
    pub staged: Option<(&'a str, &'a Document)>,
}

// ---------------------------------------------------------------------- projection

/// Appends the page of `doc` to the state contribution `t`: one `docs.b.<row>` panel per
/// block under `docs.page`. `cell` supplies var values; `None` (not yet elaborated) shows
/// expressions without values. A plot slot with data becomes a `graph` node, recorded in
/// `graphs` as `(node id, expression)` for the painter; a staged edit adds a banner with
/// Accept / Reject and its new expression beside each var it changes (the docs half of the
/// visual diff).
///
/// Cost: O(blocks) text per call; the shell reparses only when the text changed.
// ponytail: rebuilt on every input frame like the rest of the state contribution; cache the
// text by (document epoch, values, page state) if pages grow to thousands of blocks.
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
                    node(t, &format!("{id}.hint"), "label", &id);
                    attr(
                        t,
                        "label",
                        "Markdown with $math$ and $$display math$$; Ctrl+Enter applies, Esc cancels",
                    );
                    attr(t, "style", "docs-hint");
                }
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
fn var_expr<'d>(doc: &'d Document, name: &str) -> Option<&'d str> {
    let b = doc.doc_blocks();
    let s = doc.strings();
    (0..b.kind.len())
        .find(|&r| b.kind[r] == RecordKind::Var && s.get(b.fields[r][0]) == name)
        .map(|r| s.get(b.fields[r][1]))
}

/// A prose block: prose segments and `plot` slots in source order. A slot whose expression
/// has data in `plots` is a `graph` node (recorded in `graphs`); the others say why empty.
fn prose(
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
fn var_row(
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

/// Value and D20 flag of own var `v` (`name` guards against a stale state).
pub fn var_value<'c>(cell: &'c CellState, name: &str, v: usize) -> Option<(&'c Val, bool)> {
    let g = &cell.graph;
    let n = *g.var_name.get(v)?;
    if cell.names.get(n) != name {
        return None;
    }
    let node = g.params as usize + v;
    Some((g.vals.get(node)?, *g.unverified.get(node)?))
}

/// Engineering text with the unit glued on (`5.2µm`, `20µA`, `15`).
pub fn value_text(val: &Val, unit: &str) -> String {
    match val.as_f64() {
        Some(x) if matches!(val, Val::Num(_)) => {
            format!("{}{unit}", schemify_wave::format_si(x, 4))
        }
        _ => {
            let mut s = String::new();
            val.write_text(&mut s);
            if !unit.is_empty() {
                s.push(' ');
                s.push_str(unit);
            }
            s
        }
    }
}

/// D13 status of a var against its `min`/`max` (expressions in the cell scope): `None` when
/// inside, else why not. A bound that does not evaluate is reported as such.
fn bound_status(
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
fn eval_in_cell(src: &str, cell: &CellState) -> Option<f64> {
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

fn node(t: &mut String, id: &str, kind: &str, parent: &str) {
    let _ = writeln!(t, "node {id} {kind}");
    let _ = writeln!(t, "  parent {parent}");
}

fn attr(t: &mut String, key: &str, value: &str) {
    let _ = writeln!(t, "  {key} {}", quote(value));
}

/// Quotes a record value with the format §2 escapes (multi-line prose survives).
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------- edits

fn target(doc: &Document, row: u32) -> Option<Target> {
    let b = doc.doc_blocks();
    let kind = *b.kind.get(row as usize)?;
    let key = doc.strings().get(b.fields[row as usize][0]);
    Some(Target::new(kind, key))
}

/// Ops setting var `row`'s expression to `expr` (D20: a stale `cached` value goes with the
/// old expression). Empty when nothing changes or `row` is not a var.
pub fn var_ops(doc: &Document, row: u32, expr: &str) -> Vec<Op> {
    let expr = expr.trim();
    let b = doc.doc_blocks();
    if kind_of(doc, row) != Some(RecordKind::Var) || expr.is_empty() {
        return Vec::new();
    }
    let s = doc.strings();
    if s.get(b.fields[row as usize][1]) == expr {
        return Vec::new();
    }
    let t = target(doc, row).expect("row checked");
    let mut ops = vec![Op::SetField {
        target: t.clone(),
        index: 1,
        value: expr.to_owned(),
    }];
    if doc
        .pool()
        .find_attr(s, b.attrs[row as usize], "cached")
        .is_some()
    {
        ops.push(Op::RemoveAttr {
            target: t,
            key: "cached".into(),
            first: None,
        });
    }
    ops
}

/// Ops replacing prose `row`'s text (an all-blank text deletes the block).
pub fn prose_ops(doc: &Document, row: u32, text: &str) -> Vec<Op> {
    if kind_of(doc, row) != Some(RecordKind::Prose) || prose_text(doc, row) == text {
        return Vec::new();
    }
    let t = target(doc, row).expect("row checked");
    if text.trim().is_empty() {
        return vec![Op::Delete(t)];
    }
    let mut rec = doc.record(Loc {
        table: TableId::DocBlocks,
        row,
    });
    rec.verbatim = text.lines().map(|l| l.trim_end().to_owned()).collect();
    vec![Op::Put { target: t, rec }]
}

/// Row a new block goes to: after the selection, else at the end.
pub fn insert_row(doc: &Document, sel: Option<u32>) -> u32 {
    let n = doc.doc_blocks().kind.len() as u32;
    sel.map_or(n, |s| (s + 1).min(n))
}

/// Op inserting a new prose block at `row`.
pub fn add_prose(row: u32) -> Vec<Op> {
    let rec = RecordBuf::new(RecordKind::Prose)
        .field(NEW_ID)
        .verbatim("New text: markdown with $inline$ and $$display$$ math.");
    vec![Op::Insert { rec, at: Some(row) }]
}

/// Op inserting a new var `v<k>` (the first free name) with value 0 at `row`.
pub fn add_var(doc: &Document, row: u32) -> Vec<Op> {
    let b = doc.doc_blocks();
    let s = doc.strings();
    let taken = |name: &str| {
        (0..b.kind.len()).any(|r| b.kind[r] == RecordKind::Var && s.get(b.fields[r][0]) == name)
    };
    // Bounded by the block capacity: at most n names are taken.
    let name = (1..=b.kind.len() + 1)
        .map(|k| format!("v{k}"))
        .find(|n| !taken(n))
        .expect("a free name among n + 1 candidates");
    let rec = RecordBuf::new(RecordKind::Var).field(&name).field("0");
    vec![Op::Insert { rec, at: Some(row) }]
}

/// Op moving block `row` by one (`up`), or nothing at the edge.
pub fn move_block(doc: &Document, row: u32, up: bool) -> Vec<Op> {
    let n = doc.doc_blocks().kind.len() as u32;
    let to = if up {
        row.checked_sub(1)
    } else {
        Some(row + 1)
    };
    match (target(doc, row), to.filter(|&to| to < n)) {
        (Some(t), Some(to)) => vec![Op::Reorder { target: t, to }],
        _ => Vec::new(),
    }
}

/// Op deleting block `row`.
pub fn delete_block(doc: &Document, row: u32) -> Vec<Op> {
    target(doc, row).map(Op::Delete).into_iter().collect()
}

// ---------------------------------------------------------------------- D20 cache

/// Ops writing `cached <value> <hash>` on every own var whose expression calls plugin
/// functions and whose current value a loaded plugin computed (not unverified, not an
/// error), when the stored pair differs. `hash` maps the program's sorted, distinct
/// `ns.f` names to the content hash of the plugins serving them (`None`: one is missing).
pub fn cache_ops(
    doc: &Document,
    cell: &CellState,
    hash: impl Fn(&[String]) -> Option<String>,
) -> Vec<Op> {
    let b = doc.doc_blocks();
    let s = doc.strings();
    let mut ops = Vec::new();
    let mut v = 0usize;
    for r in 0..b.kind.len() {
        if b.kind[r] != RecordKind::Var {
            continue;
        }
        let k = v;
        v += 1;
        let (name, expr) = (s.get(b.fields[r][0]), s.get(b.fields[r][1]));
        let Ok(prog) = Program::compile(expr, Mode::Scalar) else {
            continue;
        };
        let mut fns: Vec<String> = prog.plugin_fns().map(|(n, f)| format!("{n}.{f}")).collect();
        if fns.is_empty() {
            continue;
        }
        fns.sort_unstable();
        fns.dedup();
        let Some(h) = hash(&fns) else { continue };
        let Some((val, unverified)) = var_value(cell, name, k) else {
            continue;
        };
        if unverified || val.is_err() || matches!(val, Val::Str(_) | Val::Cnum(..)) {
            continue;
        }
        let mut text = String::new();
        val.write_text(&mut text);
        let stored = doc.pool().find_attr(s, b.attrs[r], "cached");
        let same = stored.is_some_and(|c| c.len() == 2 && s.get(c[0]) == text && s.get(c[1]) == h);
        if !same {
            ops.push(Op::SetAttr {
                target: Target::new(RecordKind::Var, name),
                key: "cached".into(),
                values: vec![text, h],
            });
        }
    }
    debug_assert!(ops.len() <= v);
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_parse_from_node_ids_and_quotes_escape_lines() {
        assert_eq!(row_of("docs.b.12.expr"), Some(12));
        assert_eq!(row_of("docs.b.3"), Some(3));
        assert_eq!(row_of("docs.page"), None);
        assert_eq!(row_of("docs.b.x"), None);
        assert_eq!(quote("a\"b\\\nc\t"), "\"a\\\"b\\\\\\nc\\t\"");
    }

    /// Prose names images inside the project only (the plugin ABI's rule); every way out is
    /// refused before any file is touched.
    #[test]
    fn image_paths_stay_inside_the_project() {
        let root = Path::new("proj");
        assert_eq!(image_path(root, "img/a.png"), Ok(root.join("img/a.png")));
        for bad in [
            "",
            "../a.png",
            "img/../../a.png",
            "/etc/passwd",
            "./a.png",
            "img//a.png",
            "img\\a.png",
            "C:/a.png",
            "a\0.png",
        ] {
            assert!(image_path(root, bad).is_err(), "{bad:?}");
        }
    }

    /// The size check reads the PNG header only, so a huge declared image is refused before
    /// its pixels are allocated; anything else is not a PNG.
    #[test]
    fn png_size_reads_the_header() {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&100_000u32.to_be_bytes());
        b.extend_from_slice(&7u32.to_be_bytes());
        assert_eq!(png_size(&b), Some((100_000, 7)));
        assert_eq!(png_size(&b[..23]), None, "truncated");
        b[1] = b'J';
        assert_eq!(png_size(&b), None, "not the signature");
        assert_eq!(png_size(b"GIF89a"), None);
    }

    #[test]
    fn values_read_in_engineering_units() {
        assert_eq!(value_text(&Val::Num(5.2e-6), "m"), "5.2µm");
        assert_eq!(value_text(&Val::Int(15), ""), "15");
        assert_eq!(value_text(&Val::Bool(true), ""), "true");
    }
}
