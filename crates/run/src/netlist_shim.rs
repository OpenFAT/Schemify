//! Stand-in for `schemify_netlist::emit` until the netlist crate lands on `m1`; same
//! signature (`emit(&Elab, Dialect) -> Netlist`), ngspice-flavoured output only.
//!
//! `ponytail:` covers what the run tests need (bench `.param`, corner lines, globals,
//! leaf template lines, subckts, code blocks, bodies). Delete this module and switch
//! `crate::prepare` to `schemify_netlist::emit` when netlist merges.

use schemify_elab::{CornerLine, Dialect, Elab, Ignore, Name, Target};
use schemify_expr::{Scratch, Template, Value};
use std::fmt::Write;

/// Netlist text (the real crate also returns the name map).
pub(crate) struct Netlist {
    pub(crate) text: String,
}

/// Emits `e` as an ngspice netlist.
pub(crate) fn emit(e: &Elab, dialect: Dialect) -> Netlist {
    assert!(
        matches!(dialect, Dialect::Ngspice | Dialect::Xyce),
        "shim emits SPICE only"
    );
    let n = |x: Name| e.names.get(x);
    let mut out = String::from("* schemify netlist\n");
    let b = &e.bench;
    for k in 0..b.var_name.len() {
        writeln!(out, ".param {}={}", n(b.var_name[k]), n(b.var_text[k])).expect("String");
    }
    for l in 0..b.corner_kind.len() {
        let args: Vec<&str> = b.corner_args
            [b.corner_start[l] as usize..b.corner_start[l + 1] as usize]
            .iter()
            .map(|a| n(*a))
            .collect();
        let word = match b.corner_kind[l] {
            CornerLine::Lib => ".lib",
            CornerLine::Include => ".include",
            CornerLine::Temp => ".temp",
        };
        writeln!(out, "{word} {}", args.join(" ")).expect("String");
    }
    let globals: Vec<&str> = e
        .globals
        .iter()
        .map(|g| n(*g))
        .filter(|g| *g != "0")
        .collect();
    if !globals.is_empty() {
        writeln!(out, ".global {}", globals.join(" ")).expect("String");
    }
    if e.top != u32::MAX {
        insts(e, e.top, &mut out);
    }
    for c in 0..b.code_name.len() {
        for l in &b.code_lines[b.code_start[c] as usize..b.code_start[c + 1] as usize] {
            out.push_str(n(*l));
            out.push('\n');
        }
    }
    for d in 0..e.defs.cell.len() as u32 {
        if d == e.top || !e.defs.used[d as usize] {
            continue;
        }
        let ports: Vec<&str> = e.ports(d).iter().map(|p| n(*p)).collect();
        writeln!(
            out,
            ".subckt {} {}",
            n(e.defs.name[d as usize]),
            ports.join(" ")
        )
        .expect("String");
        insts(e, d, &mut out);
        writeln!(out, ".ends {}", n(e.defs.name[d as usize])).expect("String");
    }
    for i in 0..e.impls.cell.len() {
        for l in &e.impls.body[e.impls.body_start[i] as usize..e.impls.body_start[i + 1] as usize] {
            out.push_str(n(*l));
            out.push('\n');
        }
    }
    out.push_str(".end\n");
    assert!(out.ends_with(".end\n"));
    Netlist { text: out }
}

fn insts(e: &Elab, d: u32, out: &mut String) {
    let n = |x: Name| e.names.get(x);
    for r in e.insts_of(d) {
        if e.insts.ignore[r] != Ignore::No {
            continue;
        }
        let conns: Vec<&str> = e
            .conns(r)
            .iter()
            .map(|c| if *c == Name::NONE { "?" } else { n(*c) })
            .collect();
        let name = n(e.insts.name[r]);
        let leaf = match e.insts.target[r] {
            Target::Def(c) => {
                writeln!(
                    out,
                    "X{name} {} {}",
                    conns.join(" "),
                    n(e.defs.name[c as usize])
                )
                .expect("String");
                continue;
            }
            Target::Leaf(i) => i as usize,
        };
        let cell = &e.cells[e.impls.cell[leaf] as usize];
        let k = e.impls.index[leaf] as usize;
        let tpl = match e.impls.template[leaf] {
            Name::NONE if n(cell.impl_lang[k]) == "veriloga" => "N{name} {pins} {model}",
            Name::NONE => "X{name} {pins} {cell}",
            t => n(t),
        };
        let Ok(t) = Template::parse(tpl) else {
            writeln!(out, "* bad template for {name}").expect("String");
            continue;
        };
        // Identifier values: pins by name (bus bits space-joined), params, the rest.
        let mut owned: Vec<(String, String)> = vec![
            ("name".into(), name.into()),
            ("pins".into(), conns.join(" ")),
            ("cell".into(), n(cell.stem).into()),
        ];
        if cell.impl_model[k] != Name::NONE {
            owned.push(("model".into(), n(cell.impl_model[k]).into()));
        }
        for p in 0..cell.pin_name.len() {
            let w = (cell.pin_bit_start[p + 1] - cell.pin_bit_start[p]) as usize;
            let off = cell.pin_off[p];
            if off != u32::MAX {
                let bits = &conns[off as usize..off as usize + w];
                owned.push((n(cell.pin_name[p]).into(), bits.join(" ")));
            }
        }
        let params = e.params_of(r);
        let kv: Vec<String> = params
            .clone()
            .map(|q| format!("{}={}", n(e.insts.param_name[q]), n(e.insts.param_text[q])))
            .collect();
        owned.push(("params".into(), kv.join(" ")));
        let mut inputs = Vec::new();
        let bound = t.program().bind(&mut inputs, |id| {
            if let Some(q) = params.clone().find(|&q| n(e.insts.param_name[q]) == id) {
                return e.insts.param_val[q].value();
            }
            owned
                .iter()
                .find(|(k, _)| k == id)
                .map(|(_, v)| Value::Str(v))
        });
        let mut line = String::new();
        match bound.and_then(|()| t.render(&inputs, &mut Scratch::default(), &mut line)) {
            Ok(()) => {
                out.push_str(&line);
                out.push('\n');
            }
            Err(err) => writeln!(out, "* {name}: {err}").expect("String"),
        }
    }
}
