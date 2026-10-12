//! Cadence `cdfDump` reader (D14). **Untested against real dumps**: no PDK dump is
//! available, so this follows the published syntax only (Cadence *Component Description
//! Format User Guide*, product version 6.1.4, Feb 2011: `cdfDump`/`cdfDumpAll` p. 43 and
//! p. 165, `cdfCreateParam` keywords p. 161, attribute table p. 64, and the full
//! `analogLib nbsim` dump in appendix C, pp. 207–212). Fixtures are synthetic.
//!
//! A dump is SKILL code. This reader does not evaluate it; it recognises:
//! - `LIBRARY = "lib"` and `CELL = "cell"` assignments, which open a cell (a
//!   `cdfDumpAll` file holds many);
//! - `cdfCreateParam( cdfId ?key value … )` calls, one parameter each;
//! - `cdfId->formInitProc = "…"` / `cdfId->doneProc = "…"` cell properties.
//!
//! Per parameter it extracts `name prompt type defValue choices units callback`; every
//! other `?key` is kept raw in [`CdfDump::extras`], never dropped. Callbacks and cell
//! procs are SKILL code Schemify cannot run, so each non-empty one is listed in
//! [`CdfDump::review`].

use crate::{Span, push_span};

/// A parameter's `?type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CdfType {
    /// `"string"` (numbers too, with `?parseAsNumber "yes"`).
    String,
    /// `"int"`.
    Int,
    /// `"float"`.
    Float,
    /// `"radio"`: one of `?choices`.
    Radio,
    /// `"cyclic"`: one of `?choices`.
    Cyclic,
    /// `"boolean"`.
    Boolean,
    /// `"button"`.
    Button,
    /// `"netSet"`.
    NetSet,
    /// Any other value (raw text in [`CdfDump::extras`] under `type`).
    Other,
    /// No `?type`.
    Missing,
}

/// Cells opened by `CELL = "…"`. Arena: [`CdfDump::text`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cells {
    /// Library name in effect (`LIBRARY = "…"`), empty if none.
    pub lib: Vec<Span>,
    /// Cell name, empty for parameters before any `CELL`.
    pub cell: Vec<Span>,
    /// 1-based line of the `CELL` assignment (0 for the implicit cell).
    pub line: Vec<u32>,
}

/// One row per `cdfCreateParam` call. Arena: [`CdfDump::text`]; absent fields are empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CdfParams {
    /// Row in [`CdfDump::cells`].
    pub cell: Vec<u32>,
    /// `?name`.
    pub name: Vec<Span>,
    /// `?prompt`.
    pub prompt: Vec<Span>,
    /// `?type`.
    pub ty: Vec<CdfType>,
    /// `?defValue`: the string's text, or the raw SKILL atom (`100.0`, `7u`, `t`, `nil`).
    pub def_value: Vec<Span>,
    /// `?units` (`lengthMetric`, `capacitance`, …).
    pub units: Vec<Span>,
    /// `?callback` SKILL code.
    pub callback: Vec<Span>,
    /// `[first, count]` into [`CdfDump::choices`].
    pub choices: Vec<[u32; 2]>,
    /// 1-based line of `cdfCreateParam`.
    pub line: Vec<u32>,
}

/// Every `?key value` of a parameter not given its own column. Arena: [`CdfDump::text`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extras {
    /// Row in [`CdfDump::params`].
    pub param: Vec<u32>,
    /// Keyword without `?` (`display`, `editable`, `parseAsNumber`, …).
    pub key: Vec<Span>,
    /// Value: string text, or raw SKILL source for anything else.
    pub value: Vec<Span>,
}

/// What a review entry points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewKind {
    /// A parameter `?callback` (field: row in [`CdfDump::params`]).
    Callback,
    /// A cell `formInitProc` (field: row in [`CdfDump::cells`]).
    FormInitProc,
    /// A cell `doneProc` (field: row in [`CdfDump::cells`]).
    DoneProc,
}

/// Dynamic SKILL behaviour Schemify cannot run, to be reviewed by a person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Review {
    /// What it is.
    pub kind: ReviewKind,
    /// Row in the table [`ReviewKind`] names.
    pub row: u32,
    /// 1-based line.
    pub line: u32,
}

/// Why a construct was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CdfIssueKind {
    /// `cdfCreateParam` without `?name`; the call was dropped.
    ParamWithoutName,
    /// A `?key` with no value before `)`.
    MissingValue,
    /// End of input inside a string, comment or call.
    Unterminated,
}

/// A dropped construct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CdfIssue {
    /// 1-based line.
    pub line: u32,
    /// What went wrong.
    pub kind: CdfIssueKind,
}

/// Everything read from one dump.
///
/// Invariant (asserted on exit of [`read`]): `param_calls == params.name.len() +
/// dropped params`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CdfDump {
    /// Arena for every [`Span`] here.
    pub text: String,
    /// Cells.
    pub cells: Cells,
    /// Parameters.
    pub params: CdfParams,
    /// `?choices` elements.
    pub choices: Vec<Span>,
    /// Other `?key value` pairs.
    pub extras: Extras,
    /// Callbacks and cell procs.
    pub review: Vec<Review>,
    /// Dropped constructs.
    pub issues: Vec<CdfIssue>,
    /// `cdfCreateParam` calls seen.
    pub param_calls: u32,
}

impl CdfDump {
    /// Returns `span`'s text from [`CdfDump::text`].
    pub fn str(&self, span: Span) -> &str {
        span.of(&self.text)
    }

    /// Returns parameter `p`'s choices.
    pub fn choices_of(&self, p: usize) -> impl Iterator<Item = &str> {
        let [first, count] = self.params.choices[p];
        self.choices[first as usize..(first + count) as usize]
            .iter()
            .map(|s| self.str(*s))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok {
    /// A string literal; span into the decoded arena.
    Str(Span),
    /// A bare atom; byte range in the source.
    Atom(usize, usize),
    Open,
    Close,
    Quote,
}

/// Reads a `cdfDump` / `cdfDumpAll` file. Total: unusable parts land in
/// [`CdfDump::issues`].
///
/// # Panics
///
/// Panics if `src` exceeds 2 GiB (the text arena is indexed by `u32`).
///
/// # Examples
///
/// ```
/// use interop::cdf::{read, CdfType};
/// let d = read("LIBRARY = \"base\"\nCELL = \"res\"\ncdfCreateParam( cdfId\n ?name \"r\" ?type \"radio\"\n \
///               ?defValue \"ideal\" ?choices list(\"ideal\" \"thin film\") ?callback \"myCB()\")\n");
/// assert_eq!(d.str(d.params.name[0]), "r");
/// assert_eq!(d.params.ty[0], CdfType::Radio);
/// assert_eq!(d.choices_of(0).collect::<Vec<_>>(), ["ideal", "thin film"]);
/// assert_eq!(d.review.len(), 1);
/// ```
pub fn read(src: &str) -> CdfDump {
    let mut d = CdfDump::default();
    let (toks, lines) = tokenize(src, &mut d);
    let atom = |t: &Tok| match *t {
        Tok::Atom(a, b) => Some(&src[a..b]),
        _ => None,
    };
    let mut lib = Span::default();
    let mut dropped = 0u32;
    let mut i = 0;
    while i < toks.len() {
        let name = atom(&toks[i]);
        let assigned = (toks.get(i + 1).and_then(atom) == Some("="))
            .then(|| toks.get(i + 2))
            .flatten();
        match (name, assigned) {
            (Some("LIBRARY"), Some(Tok::Str(s))) => {
                lib = *s;
                i += 3;
                continue;
            }
            (Some("CELL"), Some(Tok::Str(s))) => {
                d.cells.lib.push(lib);
                d.cells.cell.push(*s);
                d.cells.line.push(lines[i]);
                i += 3;
                continue;
            }
            (Some(n), Some(Tok::Str(s)))
                if n.ends_with("->formInitProc") || n.ends_with("->doneProc") =>
            {
                if !d.str(*s).trim().is_empty() {
                    let cell = current_cell(&mut d);
                    let kind = if n.ends_with("->doneProc") {
                        ReviewKind::DoneProc
                    } else {
                        ReviewKind::FormInitProc
                    };
                    d.review.push(Review {
                        kind,
                        row: cell,
                        line: lines[i],
                    });
                }
                i += 3;
                continue;
            }
            _ => {}
        }
        if name == Some("cdfCreateParam") && toks.get(i + 1) == Some(&Tok::Open) {
            d.param_calls += 1;
            let line = lines[i];
            let end = group_end(&toks, i + 1);
            if !param(
                src,
                &mut d,
                &toks[i + 2..end.min(toks.len())],
                &lines[i + 2..end.min(toks.len())],
                line,
            ) {
                dropped += 1;
            }
            if end >= toks.len() {
                d.issues.push(CdfIssue {
                    line,
                    kind: CdfIssueKind::Unterminated,
                });
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    assert_eq!(
        d.param_calls,
        d.params.name.len() as u32 + dropped,
        "parameter conservation"
    );
    assert!(
        d.review
            .iter()
            .all(|r| r.kind != ReviewKind::Callback || (r.row as usize) < d.params.name.len())
    );
    d
}

/// Returns the row of the cell in effect, creating the implicit empty cell if needed.
fn current_cell(d: &mut CdfDump) -> u32 {
    if d.cells.cell.is_empty() {
        d.cells.lib.push(Span::default());
        d.cells.cell.push(Span::default());
        d.cells.line.push(0);
    }
    d.cells.cell.len() as u32 - 1
}

/// Returns the index of the `Close` matching the `Open` at `open` (or `toks.len()`).
fn group_end(toks: &[Tok], open: usize) -> usize {
    let mut depth = 0u32;
    for (j, t) in toks.iter().enumerate().skip(open) {
        match t {
            Tok::Open => depth += 1,
            Tok::Close => {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
    }
    toks.len()
}

/// Returns the end (exclusive) of the value expression starting at `j`.
fn value_end(toks: &[Tok], j: usize) -> usize {
    match toks[j] {
        Tok::Quote if toks.get(j + 1) == Some(&Tok::Open) => group_end(toks, j + 1) + 1,
        Tok::Quote => (j + 2).min(toks.len()),
        Tok::Open => group_end(toks, j) + 1,
        Tok::Atom(..) if toks.get(j + 1) == Some(&Tok::Open) => group_end(toks, j + 1) + 1,
        _ => j + 1,
    }
    .min(toks.len())
}

/// Reads one `cdfCreateParam` argument list (after the `(`); returns false if dropped.
fn param(src: &str, d: &mut CdfDump, args: &[Tok], lines: &[u32], line: u32) -> bool {
    let cell = current_cell(d);
    let row = d.params.name.len() as u32;
    let (mut name, mut prompt, mut def, mut units, mut callback) = Default::default();
    let mut ty = CdfType::Missing;
    let first_choice = d.choices.len() as u32;
    let extras_mark = d.extras.param.len();
    let mut j = 0;
    while j < args.len() {
        let Tok::Atom(a, b) = args[j] else {
            j += 1;
            continue;
        };
        let Some(key) = src[a..b].strip_prefix('?') else {
            j += 1; // the cdfId argument, or junk
            continue;
        };
        if j + 1 >= args.len() {
            d.issues.push(CdfIssue {
                line: lines[j],
                kind: CdfIssueKind::MissingValue,
            });
            break;
        }
        let end = value_end(args, j + 1);
        let value = &args[j + 1..end];
        let text = value_text(src, d, value);
        match key {
            "name" => name = text,
            "prompt" => prompt = text,
            "defValue" => def = text,
            "units" => units = text,
            "callback" => callback = text,
            "type" => {
                ty = match d.str(text) {
                    "string" => CdfType::String,
                    "int" => CdfType::Int,
                    "float" => CdfType::Float,
                    "radio" => CdfType::Radio,
                    "cyclic" => CdfType::Cyclic,
                    "boolean" => CdfType::Boolean,
                    "button" => CdfType::Button,
                    "netSet" => CdfType::NetSet,
                    _ => {
                        push_extra(d, row, key, text);
                        CdfType::Other
                    }
                }
            }
            "choices" => {
                for t in value {
                    match *t {
                        Tok::Str(s) => d.choices.push(s),
                        Tok::Atom(x, y) if &src[x..y] != "list" => {
                            let s = push_span(&mut d.text, &src[x..y]);
                            d.choices.push(s);
                        }
                        _ => {}
                    }
                }
            }
            _ => push_extra(d, row, key, text),
        }
        j = end;
    }
    if name.len == 0 {
        d.choices.truncate(first_choice as usize);
        d.extras.param.truncate(extras_mark);
        d.extras.key.truncate(extras_mark);
        d.extras.value.truncate(extras_mark);
        d.issues.push(CdfIssue {
            line,
            kind: CdfIssueKind::ParamWithoutName,
        });
        return false;
    }
    if !d.str(callback).trim().is_empty() {
        d.review.push(Review {
            kind: ReviewKind::Callback,
            row,
            line,
        });
    }
    let p = &mut d.params;
    p.cell.push(cell);
    p.name.push(name);
    p.prompt.push(prompt);
    p.ty.push(ty);
    p.def_value.push(def);
    p.units.push(units);
    p.callback.push(callback);
    p.choices
        .push([first_choice, d.choices.len() as u32 - first_choice]);
    p.line.push(line);
    true
}

fn push_extra(d: &mut CdfDump, row: u32, key: &str, value: Span) {
    let k = push_span(&mut d.text, key);
    d.extras.param.push(row);
    d.extras.key.push(k);
    d.extras.value.push(value);
}

/// A lone string's text, or the raw source of any other expression.
fn value_text(src: &str, d: &mut CdfDump, value: &[Tok]) -> Span {
    if let [Tok::Str(s)] = value {
        return *s;
    }
    let mut out = String::new();
    for t in value {
        match *t {
            Tok::Str(s) => {
                out.push('"');
                out.push_str(d.str(s));
                out.push('"');
            }
            Tok::Atom(a, b) => out.push_str(&src[a..b]),
            Tok::Open => out.push('('),
            Tok::Close => out.push(')'),
            Tok::Quote => out.push('\''),
        }
        if !matches!(t, Tok::Open | Tok::Quote) {
            out.push(' ');
        }
    }
    push_span(&mut d.text, out.trim_end())
}

/// Splits SKILL source into tokens (with their 1-based lines); string literals are
/// decoded into `d.text`.
fn tokenize(src: &str, d: &mut CdfDump) -> (Vec<Tok>, Vec<u32>) {
    let b = src.as_bytes();
    let (mut toks, mut lines) = (Vec::new(), Vec::new());
    let mut line = 1u32;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_ascii_whitespace() => i += 1,
            b';' => i = src[i..].find('\n').map_or(b.len(), |n| i + n),
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = src[i + 2..].find("*/").map_or(b.len(), |n| i + 2 + n + 2);
                line += src[i..end].bytes().filter(|x| *x == b'\n').count() as u32;
                if end == b.len() && !src.ends_with("*/") {
                    d.issues.push(CdfIssue {
                        line,
                        kind: CdfIssueKind::Unterminated,
                    });
                }
                i = end;
            }
            b'(' | b')' | b'\'' => {
                toks.push(match c {
                    b'(' => Tok::Open,
                    b')' => Tok::Close,
                    _ => Tok::Quote,
                });
                lines.push(line);
                i += 1;
            }
            b'"' => {
                let start_line = line;
                let start = d.text.len();
                i += 1;
                let mut seg = i;
                let mut closed = false;
                while i < b.len() {
                    match b[i] {
                        b'"' => {
                            d.text.push_str(&src[seg..i]);
                            i += 1;
                            closed = true;
                            break;
                        }
                        b'\\' if i + 1 < b.len() => {
                            d.text.push_str(&src[seg..i]);
                            match b[i + 1] {
                                b'n' => d.text.push('\n'),
                                b't' => d.text.push('\t'),
                                _ => {}
                            }
                            // Other escapes keep the char itself: it starts the next segment.
                            seg = if matches!(b[i + 1], b'n' | b't') {
                                i + 2
                            } else {
                                i + 1
                            };
                            if b[i + 1] == b'\n' {
                                line += 1;
                            }
                            i += 2;
                        }
                        b'\n' => {
                            line += 1;
                            i += 1;
                        }
                        _ => i += 1,
                    }
                }
                if !closed {
                    d.text.push_str(&src[seg.min(b.len())..]);
                    d.issues.push(CdfIssue {
                        line: start_line,
                        kind: CdfIssueKind::Unterminated,
                    });
                }
                toks.push(Tok::Str(crate::Span::new(start, d.text.len())));
                lines.push(start_line);
            }
            _ => {
                let start = i;
                while i < b.len()
                    && !b[i].is_ascii_whitespace()
                    && !matches!(b[i], b'(' | b')' | b'"' | b'\'' | b';')
                {
                    i += 1;
                }
                toks.push(Tok::Atom(start, i));
                lines.push(line);
            }
        }
    }
    assert_eq!(toks.len(), lines.len());
    (toks, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped after the appendix-C `nbsim` dump of the CDF User Guide (abridged).
    const NBSIM: &str = r#"/****************************************************/
LIBRARY = "analogLib"
CELL = "nbsim"
/****************************************************/
let( ( libId cellId cdfId )
    unless( cellId = ddGetObj( LIBRARY CELL )
        error( "Could not get cell %s." CELL )
    )
    when( cdfId = cdfGetBaseCellCDF( cellId )
        cdfDeleteCDF( cdfId )
    )
    cdfId = cdfCreateBaseCellCDF( cellId )
;;; Parameters
cdfCreateParam( cdfId
    ?name "model"
    ?prompt "Model name"
    ?defValue ""
    ?type "string"
    ?display "artParameterInToolDisplay('model)"
    ?parseAsCEL "yes"
)
 cdfCreateParam( cdfId
     ?name "w"
     ?prompt "Width"
     ?units "lengthMetric"
     ?defValue ""
     ?type "string"
     ?display "artParameterInToolDisplay('w)"
     ?parseAsNumber "yes"
     ?parseAsCEL "yes"
 )
;;; Simulator Information
cdfId->simInfo = list( nil )
cdfId->simInfo->spectre = '( nil
termOrder                (D G S progn(bn))
termMapping              (nil D d G g S s B b)
)
;;; Properties
cdfId->formInitProc             = ""
cdfId->doneProc                 = "myDone()"
cdfId->buttonFieldWidth         = 340
cdfSaveCDF( cdfId )
)
"#;

    #[test]
    fn appendix_c_shape() {
        let d = read(NBSIM);
        assert!(d.issues.is_empty(), "{:?}", d.issues);
        assert_eq!(
            (d.str(d.cells.lib[0]), d.str(d.cells.cell[0])),
            ("analogLib", "nbsim")
        );
        assert_eq!(
            d.params.name.iter().map(|s| d.str(*s)).collect::<Vec<_>>(),
            ["model", "w"]
        );
        assert_eq!(d.str(d.params.units[1]), "lengthMetric");
        assert_eq!(d.params.ty, [CdfType::String, CdfType::String]);
        let extras: Vec<_> = (0..d.extras.key.len())
            .map(|e| {
                (
                    d.extras.param[e],
                    d.str(d.extras.key[e]),
                    d.str(d.extras.value[e]),
                )
            })
            .collect();
        assert_eq!(
            extras[0],
            (0, "display", "artParameterInToolDisplay('model)")
        );
        assert_eq!(extras.len(), 5);
        assert_eq!(
            d.review,
            [Review {
                kind: ReviewKind::DoneProc,
                row: 0,
                line: 40
            }]
        );
    }

    #[test]
    fn user_guide_examples() {
        // p. 160 forms: unquoted type atoms, quoted lists, numeric and boolean defaults.
        let src = "cdfParamId = cdfCreateParam(cdfDataId\n?type \"cyclic\"\n?name \"maskLayoutViewName\"\n?prompt \"Type of layout\"\n?choices ('(\"layout\" \"layout.pwr\"))\n?defValue \"layout\"\n?callback \"mosLayout( )\")\n\
                   cdfParamId = cdfCreateParam(cdfDataId ?type boolean ?name \"bend\" ?prompt \"PCell add bend?\" ?defValue nil ?display nil)\n\
                   cdfParamId = cdfCreateParam(cdfDataId ?type \"string\" ?name \"w\" ?defValue 7u ?use \"cdfgData->a->value == \\\"thin\\\"\")\n\
                   cdfCreateParam(cdfDataId ?prompt \"no name\" ?type \"float\")\ncdfCreateParam(cdfDataId ?name \"x\" ?type";
        let d = read(src);
        assert_eq!(d.param_calls, 5);
        assert_eq!(d.params.name.len(), 4);
        assert_eq!(
            d.choices_of(0).collect::<Vec<_>>(),
            ["layout", "layout.pwr"]
        );
        assert_eq!(
            (d.params.ty[1], d.str(d.params.def_value[1])),
            (CdfType::Boolean, "nil")
        );
        assert_eq!(d.str(d.params.def_value[2]), "7u");
        let use_row = (0..d.extras.key.len())
            .find(|&e| d.str(d.extras.key[e]) == "use")
            .unwrap();
        assert_eq!(
            d.str(d.extras.value[use_row]),
            "cdfgData->a->value == \"thin\""
        );
        assert_eq!(
            d.review,
            [Review {
                kind: ReviewKind::Callback,
                row: 0,
                line: 1
            }]
        );
        let kinds: Vec<_> = d.issues.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            [
                CdfIssueKind::ParamWithoutName,
                CdfIssueKind::MissingValue,
                CdfIssueKind::Unterminated
            ]
        );
        assert_eq!(read("\"open").issues[0].kind, CdfIssueKind::Unterminated);
    }
}
