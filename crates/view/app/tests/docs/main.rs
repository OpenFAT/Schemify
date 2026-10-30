//! The docs page end to end, headless (D19, D20, D21): the OTA fixture's doc blocks beside its
//! schematic, edits through the real frame spine (click a field, type, Enter), the D20 cache
//! paths, and a CPU golden of the page. Oracles: the `gmid` plugin's own answers, the
//! canonical file text (undo restores it byte for byte), and a fresh `CellState` build.

mod cache;
#[path = "../common/mod.rs"]
mod common;
mod prose;
mod values;

use common::{Session, Ui};
use schemify_elab::{CellState, Val};
use schemify_expr::{Scratch, Template, Value};

/// The OTA fixture's module, as the manifest loads it.
const WASM: &[u8] = include_bytes!("../fixtures/ota/plugins/gmid_lut.wasm");

fn open(tag: &str) -> Session {
    let mut s = Session::open("ota", "ota.shcomp", tag, Ui::Default);
    s.frame(Default::default());
    s
}

/// Value and unverified flag of own var `name`.
fn var(s: &Session, name: &str) -> (Val, bool) {
    let cell = s.ed.cell().expect("elaborated");
    var_in(cell, name)
}

fn var_in(cell: &CellState, name: &str) -> (Val, bool) {
    let g = &cell.graph;
    let v = g
        .var_name
        .iter()
        .position(|n| cell.names.get(*n) == name)
        .unwrap_or_else(|| panic!("var {name}"));
    let node = g.params as usize + v;
    (g.vals[node].clone(), g.unverified[node])
}

fn num(s: &Session, name: &str) -> f64 {
    var(s, name)
        .0
        .as_f64()
        .unwrap_or_else(|| panic!("{name} = {:?}", var(s, name)))
}

/// Evaluated value of param `param` of the instance named `inst`.
fn inst_param(s: &Session, inst: &str, param: &str) -> (Val, bool) {
    let (h, _) = s.ed.active().expect("a cell is open");
    let doc =
        s.ed.session()
            .project
            .doc(h)
            .expect("the open cell's document is loaded");
    let st = doc.strings();
    let i = (0..doc.instances().id.len())
        .find(|&i| st.get(doc.instances().name[i]) == inst)
        .unwrap_or_else(|| panic!("instance {inst}"));
    let cell = s.ed.cell().expect("a cell is open");
    let iface = &cell.ifaces[cell.inst_iface[i] as usize];
    let k = iface
        .param_name
        .iter()
        .position(|n| cell.names.get(*n) == param)
        .expect("the interface declares the parameter");
    let node = cell
        .graph
        .inst_param(i, k)
        .expect("the instance binds the parameter") as usize;
    (cell.graph.vals[node].clone(), cell.graph.unverified[node])
}

/// Doc block row of var `name`.
fn row(s: &Session, name: &str) -> u32 {
    let (h, _) = s.ed.active().expect("a cell is open");
    let doc =
        s.ed.session()
            .project
            .doc(h)
            .expect("the open cell's document is loaded");
    let b = doc.doc_blocks();
    (0..b.kind.len())
        .find(|&r| doc.strings().get(b.fields[r][0]) == name)
        .unwrap_or_else(|| panic!("block {name}")) as u32
}

/// Replaces var `name`'s expression through its page field: click, select all, type, Enter.
fn edit_var(s: &mut Session, name: &str, expr: &str) {
    let r = row(s, name);
    s.click_node(&format!("docs.b.{r}.expr"));
    s.key("Ctrl+A");
    s.type_text(expr);
    s.key("Enter");
}

/// `cached` attribute values of var `name` in the file text.
fn cached(text: &str, name: &str) -> Option<(f64, String)> {
    cached_of(text, "var", name)
}

/// `cached` attribute values of the `word` record (`var`, `param`) named `name`.
fn cached_of(text: &str, word: &str, name: &str) -> Option<(f64, String)> {
    let mut lines = text
        .lines()
        .skip_while(|l| !l.starts_with(&format!("{word} {name} ")));
    lines.next()?;
    let line = lines
        .take_while(|l| l.starts_with("  "))
        .find(|l| l.starts_with("  cached "))?;
    let mut f = line.split_whitespace().skip(1);
    let v = schemify_expr::parse_num(f.next()?).ok()?;
    Some((v, f.next()?.to_owned()))
}

/// The content hash the app writes for the fixture plugin (first 8 bytes of its sha256).
fn plugin_hash() -> String {
    let mut h = schemify_doc::hex(&schemify_doc::sha256(WASM));
    h.truncate(16);
    h
}

fn eng(x: f64) -> String {
    let t = Template::parse("{eng(x)}").expect("the template parses");
    let mut inputs = Vec::new();
    t.program()
        .bind(&mut inputs, |_| Some(Value::Num(x)))
        .expect("the template's names bind");
    let mut out = String::new();
    t.render(&inputs, &mut Scratch::default(), &mut out)
        .expect("the template renders");
    out
}

/// Node ids under the docs page, in composition order.
fn page_nodes(s: &Session) -> Vec<String> {
    let sh = &s.ed.shell();
    let n = &sh.tables().nodes;
    (0..n.len())
        .map(|i| sh.strings().get(n.id()[i]).to_owned())
        .filter(|id| id.starts_with("docs.b.") || id == "docs.empty")
        .collect()
}

fn label_of(s: &Session, id: &str) -> String {
    let sh = &s.ed.shell();
    let n = &sh.tables().nodes;
    let i = n
        .find(sh.strings().find(id).unwrap_or_else(|| panic!("{id}")))
        .unwrap_or_else(|| panic!("{id} composed"));
    sh.strings()
        .get(n.prop(schemify_ui::Prop::Label)[i as usize])
        .to_owned()
}

fn label_of_opt(s: &Session, id: &str) -> Option<String> {
    let sh = &s.ed.shell();
    let n = &sh.tables().nodes;
    let i = n.find(sh.strings().find(id)?)?;
    let l = n.prop(schemify_ui::Prop::Label)[i as usize];
    (!l.is_none()).then(|| sh.strings().get(l).to_owned())
}
