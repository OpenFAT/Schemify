//! Static lint of the shipped SKILL (spec `interop.md` §5.1): every called function is a
//! special form, an interpreter builtin, a procedure of the script set or a Cadence function
//! the mock implements, and every call's arity matches its signature (builtins: the table
//! below, from the *SKILL Language Reference*; Cadence: `mockdb::SIGNATURES`; procedures:
//! their own parameter lists). Also: procedure names start with `Sfy`, `setq` targets are
//! locals or `sfy`-prefixed globals, `lambda` appears only as an argument of `sort`/`mapcar`,
//! no `eval`/`load`/`sstatus`, and `evalstring` exactly once (the CDF callback, §5.3).

mod skill;

use std::collections::{BTreeMap, BTreeSet};

use skill::mockdb::{SIGNATURES, arity};
use skill::{V, defined_by_interp, read_all};

/// (builtin, min args, max args or None for any), per the SKILL Language Reference.
const BUILTIN_ARITY: &[(&str, usize, Option<usize>)] = &[
    ("list", 0, None),
    ("log", 1, Some(1)),
    ("expt", 2, Some(2)),
    ("createDir", 1, Some(1)),
    ("cons", 2, Some(2)),
    ("car", 1, Some(1)),
    ("cdr", 1, Some(1)),
    ("cadr", 1, Some(1)),
    ("caddr", 1, Some(1)),
    ("cadddr", 1, Some(1)),
    ("cddr", 1, Some(1)),
    ("caar", 1, Some(1)),
    ("cdar", 1, Some(1)),
    ("nth", 2, Some(2)),
    ("nthelem", 2, Some(2)),
    ("length", 1, Some(1)),
    ("append", 0, None),
    ("append1", 2, Some(2)),
    ("reverse", 1, Some(1)),
    ("member", 2, Some(2)),
    ("assoc", 2, Some(2)),
    ("copy", 1, Some(1)),
    ("equal", 2, Some(2)),
    ("eq", 2, Some(2)),
    ("nequal", 2, Some(2)),
    ("neq", 2, Some(2)),
    ("null", 1, Some(1)),
    ("not", 1, Some(1)),
    ("atom", 1, Some(1)),
    ("listp", 1, Some(1)),
    ("stringp", 1, Some(1)),
    ("fixp", 1, Some(1)),
    ("floatp", 1, Some(1)),
    ("numberp", 1, Some(1)),
    ("symbolp", 1, Some(1)),
    ("plus", 1, None),
    ("difference", 1, None),
    ("times", 1, None),
    ("quotient", 1, None),
    ("minus", 1, Some(1)),
    ("add1", 1, Some(1)),
    ("sub1", 1, Some(1)),
    ("lessp", 2, Some(2)),
    ("greaterp", 2, Some(2)),
    ("leqp", 2, Some(2)),
    ("geqp", 2, Some(2)),
    ("zerop", 1, Some(1)),
    ("abs", 1, Some(1)),
    ("round", 1, Some(1)),
    ("fix", 1, Some(1)),
    ("float", 1, Some(1)),
    ("max", 1, None),
    ("min", 1, None),
    ("mod", 2, Some(2)),
    ("strcat", 0, None),
    ("strlen", 1, Some(1)),
    ("substring", 2, Some(3)),
    ("sprintf", 2, None),
    ("fprintf", 2, None),
    ("printf", 1, None),
    ("outfile", 1, Some(3)),
    ("infile", 1, Some(1)),
    ("close", 1, Some(1)),
    ("lineread", 0, Some(1)),
    ("parseString", 1, Some(3)),
    ("symbolToString", 1, Some(1)),
    ("stringToSymbol", 1, Some(1)),
    ("atoi", 1, Some(2)),
    ("atof", 1, Some(2)),
    ("sort", 2, Some(2)),
    ("alphalessp", 2, Some(2)),
    ("buildString", 1, Some(2)),
    ("mapcar", 2, Some(2)),
    ("funcall", 1, None),
    ("apply", 2, Some(2)),
    ("makeTable", 1, Some(2)),
    ("arrayref", 2, Some(2)),
    ("setarray", 3, Some(3)),
    ("error", 1, None),
    ("warn", 1, None),
    ("getShellEnvVar", 1, Some(1)),
    ("simplifyFilename", 1, Some(2)),
    ("isFile", 1, Some(2)),
    ("upperCase", 1, Some(1)),
    ("lowerCase", 1, Some(1)),
    ("strcmp", 2, Some(2)),
    ("last", 1, Some(1)),
    ("xCoord", 1, Some(1)),
    ("yCoord", 1, Some(1)),
    ("lowerLeft", 1, Some(1)),
    ("upperRight", 1, Some(1)),
    ("evenp", 1, Some(1)),
    ("oddp", 1, Some(1)),
    ("fboundp", 1, Some(1)),
    ("evalstring", 1, Some(2)),
    ("linereadstring", 1, Some(1)),
    ("charToInt", 1, Some(1)),
    ("intToChar", 1, Some(1)),
];

const FORBIDDEN: &[&str] = &["eval", "load", "sstatus", "loadstring", "errsetstring"];

/// A procedure's parameter list: required, optional count (None with @rest), keywords.
#[derive(Clone)]
struct Params {
    req: usize,
    opt: Option<usize>,
    keys: Vec<String>,
}

#[derive(Default)]
struct Lint {
    procs: BTreeMap<String, Params>,
    optional: BTreeSet<String>,
    errors: Vec<String>,
    evalstrings: usize,
    file: String,
}

fn sym(x: &V) -> Option<&str> {
    match x {
        V::Sym(s) => Some(s),
        _ => None,
    }
}

fn quoted(x: &V) -> Option<&str> {
    match x.items() {
        [h, s] if sym(h) == Some("quote") => sym(s),
        _ => None,
    }
}

/// Parameter names and the default forms of a procedure/lambda parameter list.
fn params(list: &V) -> (Params, Vec<String>, Vec<V>) {
    let mut p = Params {
        req: 0,
        opt: Some(0),
        keys: vec![],
    };
    let mut names = vec![];
    let mut defaults = vec![];
    let mut mode = 0;
    for x in list.items() {
        match sym(x) {
            Some("@optional") => mode = 1,
            Some("@key") => mode = 2,
            Some("@rest") => mode = 3,
            _ => {
                let (n, d) = match x {
                    V::Sym(s) => (s.to_string(), None),
                    _ => (
                        sym(&x.items()[0]).unwrap_or("?").to_owned(),
                        x.items().get(1).cloned(),
                    ),
                };
                match mode {
                    0 => p.req += 1,
                    1 => p.opt = p.opt.map(|k| k + 1),
                    2 => p.keys.push(n.clone()),
                    _ => p.opt = None,
                }
                names.push(n);
                defaults.extend(d);
            }
        }
    }
    (p, names, defaults)
}

impl Lint {
    fn err(&mut self, msg: String) {
        self.errors.push(format!("{}: {msg}", self.file));
    }

    fn collect(&mut self, forms: &[V]) {
        for f in forms {
            let i = f.items();
            if i.first().and_then(sym) == Some("procedure") {
                let sig = i.get(1).cloned().unwrap_or(V::Nil);
                let name = sig.items().first().and_then(sym).unwrap_or("?").to_owned();
                if !name.starts_with("Sfy") {
                    self.err(format!("procedure {name}: names start with Sfy"));
                }
                let rest = V::list(sig.items().get(1..).unwrap_or(&[]).to_vec());
                let (p, _, _) = params(&rest);
                self.procs.insert(name, p);
            }
        }
        self.find_optional(forms);
    }

    /// Names tested with (fboundp 'name): procedures of parts filled in later.
    fn find_optional(&mut self, forms: &[V]) {
        for f in forms {
            let i = f.items();
            if i.first().and_then(sym) == Some("fboundp")
                && let Some(n) = i.get(1).and_then(quoted)
            {
                self.optional.insert(n.to_owned());
            }
            if !i.is_empty() {
                self.find_optional(i);
            }
        }
    }

    fn known(&self, name: &str) -> bool {
        self.procs.contains_key(name)
            || defined_by_interp().any(|b| b == name)
            || SIGNATURES.iter().any(|(n, _)| *n == name)
            || self.optional.contains(name)
    }

    fn check_call(&mut self, name: &str, args: &[V]) {
        if FORBIDDEN.contains(&name) {
            self.err(format!("{name} is not allowed (spec 5.1)"));
            return;
        }
        if name == "evalstring" {
            self.evalstrings += 1;
        }
        let mut pos = 0;
        let mut keys = vec![];
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match sym(a) {
                Some(k) if k.starts_with('?') => {
                    keys.push(k[1..].to_owned());
                    it.next();
                }
                _ => pos += 1,
            }
        }
        let (req, max, allowed): (usize, Option<usize>, Vec<String>) =
            if let Some(p) = self.procs.get(name) {
                (p.req, p.opt.map(|o| p.req + o), p.keys.clone())
            } else if let Some((_, sig)) = SIGNATURES.iter().find(|(n, _)| *n == name) {
                let (r, m, k) = arity(sig);
                (r, Some(m), k.into_iter().map(str::to_owned).collect())
            } else if let Some((_, lo, hi)) = BUILTIN_ARITY.iter().find(|(n, _, _)| *n == name) {
                (*lo, *hi, vec![])
            } else if self.optional.contains(name) {
                return;
            } else if defined_by_interp().any(|b| b == name) {
                self.err(format!(
                    "{name}: builtin without an arity row in BUILTIN_ARITY"
                ));
                return;
            } else {
                self.err(format!("call of unknown function {name}"));
                return;
            };
        if pos < req || max.is_some_and(|m| pos > m) {
            self.err(format!(
                "{name}: called with {pos} arguments, takes {req}..{max:?}"
            ));
        }
        for k in keys {
            if !allowed.contains(&k) {
                self.err(format!("{name}: unknown keyword ?{k}"));
            }
        }
    }

    fn walk_all(&mut self, xs: &[V], scope: &mut Vec<String>, parent: &str) {
        for x in xs {
            self.walk(x, scope, parent);
        }
    }

    fn bind_and_walk(&mut self, names: Vec<String>, body: &[V], scope: &mut Vec<String>) {
        let base = scope.len();
        scope.extend(names);
        self.walk_all(body, scope, "");
        scope.truncate(base);
    }

    fn walk(&mut self, x: &V, scope: &mut Vec<String>, parent: &str) {
        let V::List(items) = x else { return };
        let Some(head) = sym(&items[0]) else {
            self.err("call of a non-symbol (redundant parentheses?)".into());
            return;
        };
        let a = &items[1..];
        match head {
            "quote" => {}
            "procedure" | "lambda" => {
                if head == "lambda" && parent != "sort" && parent != "mapcar" {
                    self.err(format!(
                        "lambda as an argument of {parent:?} (only sort/mapcar)"
                    ));
                }
                let list = if head == "procedure" {
                    V::list(
                        a.first()
                            .map(|s| s.items().get(1..).unwrap_or(&[]).to_vec())
                            .unwrap_or_default(),
                    )
                } else {
                    a.first().cloned().unwrap_or(V::Nil)
                };
                let (_, names, defaults) = params(&list);
                self.walk_all(&defaults, scope, "");
                self.bind_and_walk(names, a.get(1..).unwrap_or(&[]), scope);
            }
            "let" | "prog" => {
                let mut names = vec![];
                for b in a.first().map(V::items).unwrap_or(&[]) {
                    match b {
                        V::Sym(s) => names.push(s.to_string()),
                        _ => {
                            names.push(sym(&b.items()[0]).unwrap_or("?").to_owned());
                            if let Some(init) = b.items().get(1) {
                                self.walk(init, scope, "");
                            }
                        }
                    }
                }
                self.bind_and_walk(names, a.get(1..).unwrap_or(&[]), scope);
            }
            "setq" | "gets" => {
                for (k, v) in a.iter().enumerate() {
                    if k % 2 == 0 || head == "gets" && k == 0 {
                        let n = sym(v).unwrap_or("?");
                        if !scope.iter().any(|s| s == n) && !n.starts_with("sfy") {
                            self.err(format!(
                                "{head} of {n}: neither local nor an sfy-prefixed global"
                            ));
                        }
                        if head == "gets" {
                            self.walk_all(&a[1..], scope, "");
                            break;
                        }
                    } else {
                        self.walk(v, scope, "");
                    }
                }
            }
            "foreach" | "setof" | "exists" | "for" => {
                let var = a.first().and_then(sym).unwrap_or("?").to_owned();
                let (bounds, body) = if head == "for" { (2, 3) } else { (1, 2) };
                self.walk_all(&a[1..bounds.min(a.len())], scope, "");
                self.bind_and_walk(vec![var], &a[body.min(a.len())..], scope);
            }
            "cond" => {
                for clause in a {
                    self.walk_all(clause.items(), scope, "");
                }
            }
            "case" => {
                if let Some(k) = a.first() {
                    self.walk(k, scope, "");
                }
                for clause in &a[1.min(a.len())..] {
                    self.walk_all(clause.items().get(1..).unwrap_or(&[]), scope, "");
                }
            }
            "getSGq" | "dbGetq" => self.walk_all(&a[..1.min(a.len())], scope, ""),
            "setSGq" | "dbSetq" => self.walk_all(&a[..2.min(a.len())], scope, ""),
            "if" | "when" | "unless" | "and" | "or" | "progn" | "while" | "errset"
            | "unwindProtect" | "return" | "boundp" => self.walk_all(a, scope, head),
            _ => {
                if !self.known(head) && !FORBIDDEN.contains(&head) {
                    self.err(format!("call of unknown function {head}"));
                } else {
                    self.check_call(head, a);
                }
                if matches!(head, "mapcar" | "apply" | "funcall" | "sort") {
                    let f = if head == "sort" { a.get(1) } else { a.first() };
                    if let Some(n) = f.and_then(quoted)
                        && !self.known(n)
                    {
                        self.err(format!("{head} of unknown function {n}"));
                    }
                }
                self.walk_all(a, scope, head);
            }
        }
    }
}

fn lint() -> Lint {
    let mut l = Lint::default();
    let mut files: Vec<(&str, Vec<V>)> = interop::skill::PARTS
        .iter()
        .map(|(n, t)| (*n, read_all(t).unwrap_or_else(|e| panic!("{n}: {e}"))))
        .collect();
    files.push((
        "sfy_probe.il",
        read_all(interop::skill::PROBE).expect("probe reads"),
    ));
    for (n, forms) in &files {
        l.file = n.to_string();
        l.collect(forms);
    }
    for (n, forms) in &files {
        l.file = n.to_string();
        for f in forms {
            l.walk(f, &mut vec![], "");
        }
    }
    l
}

#[test]
fn every_call_is_known_and_well_formed() {
    let l = lint();
    assert!(l.errors.is_empty(), "lint:\n{}", l.errors.join("\n"));
    assert_eq!(
        l.evalstrings, 1,
        "evalstring must appear exactly once (the CDF callback)"
    );
    assert!(l.procs.contains_key("SfyExport") && l.procs.contains_key("SfyImport"));
}

#[test]
fn every_cadence_signature_checks_its_arity() {
    // A call with too few arguments signals, as Virtuoso's argument check does.
    let mut db = skill::mockdb::MockDb::default();
    for (name, sig) in SIGNATURES {
        let (req, _, _) = arity(sig);
        let r = db.call_fn(name, &[]);
        if req > 0 {
            assert!(r.is_err_and(|e| e.contains("arguments")), "{name}");
        }
    }
}

#[test]
fn the_lint_catches_what_it_should() {
    let mut l = Lint::default();
    let forms = read_all(
        r#"(procedure (SfyA x) (setq global 1) (dbCreateRect x) (nosuch 1) (car 1 2) (mapcar (lambda (y) y) x) (funcall (lambda (y) y) 1) (eval x) (schCreateWire x ?bogus 1))
           (procedure (bad) nil)"#,
    )
    .unwrap();
    l.collect(&forms);
    for f in &forms {
        l.walk(f, &mut vec![], "");
    }
    let all = l.errors.join("\n");
    for want in [
        "names start with Sfy",
        "setq of global",
        "dbCreateRect: called with 1 arguments",
        "unknown function nosuch",
        "car: called with 2",
        "lambda as an argument of \"funcall\"",
        "eval is not allowed",
        "unknown keyword ?bogus",
    ] {
        assert!(all.contains(want), "{want:?} not reported:\n{all}");
    }
}
