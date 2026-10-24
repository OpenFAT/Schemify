//! Templates (spec §6): holes, escapes, canonical numbers, netlist-context
//! identifiers bound by the caller, error atomicity.

use schemify_expr::{Code, Scratch, Template, Value};

fn render(src: &str, vars: &[(&str, Value<'_>)]) -> Result<String, schemify_expr::Error> {
    let t = Template::parse(src)?;
    let mut inputs = Vec::new();
    t.program().bind(&mut inputs, |n| {
        vars.iter().find(|(k, _)| *k == n).map(|(_, v)| *v)
    })?;
    let mut out = String::from(">");
    t.render(&inputs, &mut Scratch::default(), &mut out)?;
    Ok(out[1..].to_string())
}

#[test]
fn netlist_line() {
    let vars = [
        ("name", Value::Str("M1")),
        ("pins", Value::Str("d g s b")),
        ("model", Value::Str("nfet_01v8")),
        ("w", Value::Num(1e-6)),
        ("l", Value::Num(0.15e-6)),
        ("nf", Value::Int(2)),
    ];
    let line = render(
        "X{name} {pins} {model} w={w} l={l} nf={nf} m={nf > 1 ? 1 : 2}",
        &vars,
    )
    .unwrap();
    assert_eq!(line, "XM1 d g s b nfet_01v8 w=1e-6 l=1.5e-7 nf=2 m=1");
}

#[test]
fn escapes_strings_and_values() {
    assert_eq!(render("{{literal}} {1+1}", &[]).unwrap(), "{literal} 2");
    assert_eq!(render("}}{{", &[]).unwrap(), "}{");
    assert_eq!(render(r#"{"a}b" + "{"}"#, &[]).unwrap(), "a}b{");
    assert_eq!(render(r#"{"q\"}"}"#, &[]).unwrap(), "q\"}");
    assert_eq!(
        render("{true} {2.5k} {0.1} {1/3}", &[]).unwrap(),
        "true 2500 0.1 0.3333333333333333"
    );
    assert_eq!(render("no holes", &[]).unwrap(), "no holes");
    assert_eq!(render("", &[]).unwrap(), "");
    assert_eq!(
        render("{ eng(r) }Ω", &[("r", Value::Num(4700.0))]).unwrap(),
        "4.7kΩ"
    );
}

#[test]
fn identifiers_are_shared_across_holes() {
    let t = Template::parse("{a} {b} {a + b}").unwrap();
    assert_eq!(t.program().idents().collect::<Vec<_>>(), ["a", "b"]);
}

#[test]
fn errors_and_atomicity() {
    let e = Template::parse("R1 {1 +} x").unwrap_err();
    assert_eq!(
        (e.code, e.span.start),
        (Code::X009, 7),
        "spans are in template coordinates"
    );
    let e = Template::parse("R1 {foo(1)}").unwrap_err();
    assert_eq!((e.code, e.span.start, e.span.end), (Code::X006, 4, 10));
    let t = Template::parse("ok {x} {1/x}").unwrap();
    let mut out = String::from("keep");
    let e = t
        .render(&[Value::Int(0)], &mut Scratch::default(), &mut out)
        .unwrap_err();
    assert_eq!(e.code, Code::X010);
    assert_eq!(out, "keep", "a failed render leaves the output untouched");
    assert_eq!(render("{cplx(1, 2)}", &[]).unwrap_err().code, Code::X003);
    assert_eq!(render("{missing}", &[]).unwrap_err().code, Code::X004);
}

/// `segments` spelled back (bare holes as `<ident>`, other holes as `<?>`) against
/// hand-lexed expectations, and against `render` for templates of bare holes bound to
/// their own spelling.
#[test]
fn segments_spell_the_template() {
    use schemify_expr::Segment;
    for (src, want) in [
        ("M{name} {D}", "M<name> <D>"),
        ("{ D }{{x}}", "<D>{x}"),
        ("{{[}} {B} ] [{A}] {C}", "{[} <B> ] [<A>] <C>"),
        ("[{(B)}] {true} {A}", "[<?>] <?> <A>"),
        (r#"{"D"} {D+1} {D}"#, "<?> <?> <D>"),
        ("{pins}", "<pins>"),
        ("plain", "plain"),
    ] {
        let t = Template::parse(src).unwrap();
        let idents: Vec<&str> = t.program().idents().collect();
        let (mut spelled, mut bare) = (String::new(), true);
        for seg in t.segments() {
            match seg {
                Segment::Lit(s) => {
                    assert!(!s.is_empty());
                    spelled.push_str(s);
                }
                Segment::Hole { ident: Some(k) } => {
                    spelled.push_str(&format!("<{}>", idents[k as usize]));
                }
                Segment::Hole { ident: None } => {
                    bare = false;
                    spelled.push_str("<?>");
                }
            }
        }
        assert_eq!(spelled, want, "{src}");
        if bare {
            let own: Vec<String> = idents.iter().map(|i| format!("<{i}>")).collect();
            let vars: Vec<(&str, Value<'_>)> = idents
                .iter()
                .zip(&own)
                .map(|(i, o)| (*i, Value::Str(o)))
                .collect();
            assert_eq!(render(src, &vars).unwrap(), want, "{src}");
        }
    }
}

/// `spectre_source` in a template: a vsource line from a SPICE source spec; untranslatable
/// specs fail the render with `X009` and a non-string with `X003`.
#[test]
fn spectre_source_in_a_template() {
    let t = "{name} ({p} {n}) vsource {spectre_source(value)}";
    let line = render(
        t,
        &[
            ("name", Value::Str("V1")),
            ("p", Value::Str("in")),
            ("n", Value::Str("0")),
            (
                "value",
                Value::Str("dc 0.9 ac 1 pulse(0 1.8 1n 1n 1n 5n 10n)"),
            ),
        ],
    )
    .unwrap();
    assert_eq!(
        line,
        "V1 (in 0) vsource dc=0.9 mag=1 type=pulse val0=0 val1=1.8 delay=1e-9 rise=1e-9 \
         fall=1e-9 width=5e-9 period=1e-8"
    );
    let e = render("{spectre_source(v)}", &[("v", Value::Str("sffm(0 1 1k)"))]).unwrap_err();
    assert_eq!(e.code, Code::X009, "{e}");
    let e = render("{spectre_source(v)}", &[("v", Value::Num(1.0))]).unwrap_err();
    assert_eq!(e.code, Code::X003, "{e}");
}
