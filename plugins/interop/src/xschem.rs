//! xschem `.sch`/`.sym` record reader (research: `xschem-feature-inventory.md` §5).
//!
//! One pass over the text turns every record into a row of a per-kind table
//! ([`XschemFile`]). Coordinates become `i32` milli-units (D33); every value that is not
//! exactly representable is rounded half away from zero and listed in
//! [`XschemFile::roundings`] with its location. Rotation and flip are kept raw: mapping
//! them onto Schemify orientations is the translator's job. Nothing is normalised
//! (`L`/`N`/`B` endpoint ordering, layer ≥ `cadlayers`, `K`-over-`G`) — the tables hold
//! what the file says.
//!
//! Grammar, as documented in the research doc:
//! - A record is a tag char after any whitespace, then its fields. Numeric fields are
//!   whitespace-separated tokens and may span lines (C `scanf` semantics); after the last
//!   field the rest of the physical line is ignored.
//! - A brace string skips every character before its first `{`. Inside, `\` keeps the
//!   next char literally, the first unescaped `}` ends it, an unescaped `{` is literal and
//!   `\r` is dropped everywhere.
//! - `[` right after a `C` record opens that instance's embedded symbol; the body runs to
//!   the next line whose first non-blank char is `]`.
//!
//! Malformed records are dropped and reported in [`XschemFile::issues`]; the reader never
//! fails on content, only on size.

use crate::decimal::{self, DecimalError};
use crate::{MAX_INPUT_BYTES, Span};

/// Records one file level (top level or one embedded body) may hold (2^22, the per-cell
/// object cap in docs/dev/architecture.md). Reading stops with [`IssueKind::TooManyRecords`].
pub const MAX_RECORDS: u32 = 1 << 22;
/// Points one `P` record may carry; more is reported as [`IssueKind::PointCount`].
pub const MAX_POLYGON_POINTS: u32 = 1 << 16;

/// Which header record a [`Globals`] row came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalTag {
    /// `v {xschem version=… file_version=…}` plus an optional header text.
    Version,
    /// `G {…}`: VHDL text in a schematic, symbol attributes in a pre-1.2 symbol.
    G,
    /// `K {…}`: symbol attributes.
    K,
    /// `V {…}`: Verilog text.
    V,
    /// `S {…}`: SPICE text.
    S,
    /// `E {…}`: tEDAx text.
    E,
}

/// Header records (`v G K V S E`) in file order. Arena: [`XschemFile::strings`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Globals {
    /// Which record.
    pub tag: Vec<GlobalTag>,
    /// Brace content, file-level escapes removed.
    pub text: Vec<Span>,
    /// 1-based source line of the tag.
    pub line: Vec<u32>,
}

/// `L` lines or `B` boxes: `layer x1 y1 x2 y2 {props}`. Endpoints as written (not ordered).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quads {
    /// xschem layer number (layer 5 boxes are symbol pins).
    pub layer: Vec<u8>,
    /// `[x1, y1, x2, y2]` in milli-units, y down.
    pub xy: Vec<[i32; 4]>,
    /// Attribute string. Arena: [`XschemFile::strings`].
    pub props: Vec<Span>,
    /// 1-based source line of the tag.
    pub line: Vec<u32>,
}

/// `N x1 y1 x2 y2 {props}` wires.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wires {
    /// `[x1, y1, x2, y2]` in milli-units.
    pub xy: Vec<[i32; 4]>,
    /// Attribute string. Arena: [`XschemFile::strings`].
    pub props: Vec<Span>,
    /// 1-based source line of the tag.
    pub line: Vec<u32>,
}

/// `P layer n x1 y1 … xn yn {props}` polygons; points live in [`XschemFile::points`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Polygons {
    /// xschem layer number.
    pub layer: Vec<u8>,
    /// Index of the first point in [`XschemFile::points`].
    pub first: Vec<u32>,
    /// Point count, `1..=MAX_POLYGON_POINTS`.
    pub count: Vec<u32>,
    /// Attribute string. Arena: [`XschemFile::strings`].
    pub props: Vec<Span>,
    /// 1-based source line of the tag.
    pub line: Vec<u32>,
}

/// `A layer cx cy r start sweep {props}` arcs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arcs {
    /// xschem layer number.
    pub layer: Vec<u8>,
    /// `[cx, cy, r]` in milli-units.
    pub circle: Vec<[i32; 3]>,
    /// `[start, sweep]` in milli-degrees (start counter-clockwise from 3 o'clock).
    pub angles: Vec<[i32; 2]>,
    /// Attribute string. Arena: [`XschemFile::strings`].
    pub props: Vec<Span>,
    /// 1-based source line of the tag.
    pub line: Vec<u32>,
}

/// `T {text} x y rot flip xscale yscale {props}` texts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Texts {
    /// The text (may contain `@tokens`, `tcleval(…)`). Arena: [`XschemFile::strings`].
    pub text: Vec<Span>,
    /// Anchor in milli-units.
    pub xy: Vec<[i32; 2]>,
    /// Rotation exactly as written (xschem uses 0..=3).
    pub rot: Vec<i16>,
    /// Flip exactly as written (xschem uses 0..=1).
    pub flip: Vec<i16>,
    /// `[xscale, yscale]` in thousandths (inexact ones are in the rounding table).
    pub scale: Vec<[i32; 2]>,
    /// Attribute string. Arena: [`XschemFile::strings`].
    pub props: Vec<Span>,
    /// 1-based source line of the tag.
    pub line: Vec<u32>,
}

/// `C {symref} x y rot flip {props}` instances.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Instances {
    /// Symbol reference as written (unresolved). Arena: [`XschemFile::strings`].
    pub symref: Vec<Span>,
    /// Origin in milli-units.
    pub xy: Vec<[i32; 2]>,
    /// Rotation exactly as written.
    pub rot: Vec<i16>,
    /// Flip exactly as written.
    pub flip: Vec<i16>,
    /// Attribute string. Arena: [`XschemFile::strings`].
    pub props: Vec<Span>,
    /// 1-based source line of the tag.
    pub line: Vec<u32>,
}

/// An embedded symbol definition (`[ … ]`) bound to the instance right before it.
#[derive(Clone, Debug, PartialEq)]
pub struct Embedded {
    /// Row in the parent's [`XschemFile::instances`].
    pub instance: u32,
    /// 1-based source line of the `[`.
    pub line: u32,
    /// The block's records. Its spans index `body.strings`; its lines are file lines;
    /// its dropped records and roundings are in `body.issues` / `body.roundings`, not
    /// the parent's. xschem lets the first definition of a symref win, so a later block
    /// for the same symref is kept here but is the translator's to ignore.
    pub body: XschemFile,
}

/// Why a record was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueKind {
    /// Tag char is not one of `v G K V S E F # L B P A T N C [ ]`; skipped to end of line.
    UnknownTag,
    /// A numeric field is missing (hit `{` or end of input).
    MissingNumber,
    /// A numeric field is not a decimal/integer token.
    BadNumber,
    /// A number does not fit its column (`i32` milli-units, `i16` rot/flip).
    NumberOutOfRange,
    /// Layer is negative or above 255.
    LayerOutOfRange,
    /// Polygon point count outside `1..=MAX_POLYGON_POINTS`.
    PointCount,
    /// No `{` before end of input.
    MissingBrace,
    /// End of input inside a brace string.
    UnterminatedBrace,
    /// `[` not directly after a `C` record; the block was skipped.
    StrayEmbedOpen,
    /// `]` outside an embedded block.
    StrayEmbedClose,
    /// `[` inside an embedded block; skipped (xschem skips these too).
    NestedEmbed,
    /// End of input before the `]` closing an embedded block; the block was dropped.
    UnterminatedEmbed,
    /// [`MAX_RECORDS`] reached; the rest of this level was not read.
    TooManyRecords,
}

/// A dropped record and where it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Issue {
    /// 1-based line of the record's tag.
    pub line: u32,
    /// 1-based byte column of the record's tag.
    pub col: u32,
    /// The tag byte.
    pub tag: u8,
    /// What went wrong.
    pub kind: IssueKind,
}

/// Which kind of number a [`Rounding`] hit. D33 bounds only [`NumField::Coord`]; text
/// scales and arc angles share the milli grid but are not coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumField {
    /// A coordinate or length (x, y, radius, polygon point).
    Coord,
    /// A text `xscale`/`yscale`.
    Scale,
    /// An arc start or sweep angle (degrees).
    Angle,
}

/// One inexact number rounded onto the milli grid (D33).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rounding {
    /// 1-based line of the number token.
    pub line: u32,
    /// 1-based byte column of the number token.
    pub col: u32,
    /// What the number is.
    pub field: NumField,
    /// The stored, rounded value.
    pub milli: i32,
    /// `written − stored`, in thousandths of a milli-unit (`[-500, 500]`).
    pub error_micro: i16,
}

/// Every record of one `.sch`/`.sym` file level, one table per kind.
///
/// Invariant (asserted on exit of [`read`]): `records_read == records_kept() +
/// comments + issues.len()`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XschemFile {
    /// Arena for every [`Span`] in this level's tables (unescaped brace contents).
    pub strings: String,
    /// `v G K V S E` records.
    pub globals: Globals,
    /// `L` records.
    pub lines: Quads,
    /// `B` records.
    pub boxes: Quads,
    /// `P` records.
    pub polygons: Polygons,
    /// Polygon vertices `[x, y]` in milli-units, sliced by [`Polygons::first`]/`count`.
    pub points: Vec<[i32; 2]>,
    /// `A` records.
    pub arcs: Arcs,
    /// `T` records.
    pub texts: Texts,
    /// `N` records.
    pub wires: Wires,
    /// `C` records.
    pub instances: Instances,
    /// `[ … ]` blocks.
    pub embedded: Vec<Embedded>,
    /// `#` comment and `F` (reserved) records, ignored by xschem.
    pub comments: u32,
    /// Every record tag seen at this level (the `]` closing a block is not a record).
    pub records_read: u32,
    /// Dropped records.
    pub issues: Vec<Issue>,
    /// Numbers rounded to the milli grid.
    pub roundings: Vec<Rounding>,
}

/// Input refused before parsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// Input longer than [`MAX_INPUT_BYTES`].
    TooLarge {
        /// Input length in bytes.
        bytes: usize,
    },
}

impl XschemFile {
    /// Returns the text of `span` from this level's arena.
    pub fn str(&self, span: Span) -> &str {
        span.of(&self.strings)
    }

    /// Returns the number of records stored in the kind tables.
    pub fn records_kept(&self) -> u32 {
        let n = self.globals.tag.len()
            + self.lines.layer.len()
            + self.boxes.layer.len()
            + self.polygons.layer.len()
            + self.arcs.layer.len()
            + self.texts.text.len()
            + self.wires.xy.len()
            + self.instances.symref.len()
            + self.embedded.len();
        n as u32
    }

    /// Returns the text of the first record with `tag`, if any.
    pub fn global(&self, tag: GlobalTag) -> Option<&str> {
        let i = self.globals.tag.iter().position(|t| *t == tag)?;
        Some(self.str(self.globals.text[i]))
    }
}

/// Reads one `.sch`/`.sym` file.
///
/// Never fails on content: malformed records are dropped into
/// [`XschemFile::issues`] with their location. The returned tables hold records in file
/// order within each kind.
///
/// # Errors
///
/// [`ReadError::TooLarge`] when `src` exceeds [`MAX_INPUT_BYTES`].
///
/// # Examples
///
/// ```
/// use interop::xschem::read;
/// let f = read("v {xschem version=3.4.7 file_version=1.2}\nN 290 -240 290 -220 {lab=A}\n").unwrap();
/// assert_eq!(f.wires.xy[0], [290_000, -240_000, 290_000, -220_000]);
/// assert_eq!(f.str(f.wires.props[0]), "lab=A");
/// assert!(f.issues.is_empty() && f.roundings.is_empty());
/// ```
pub fn read(src: &str) -> Result<XschemFile, ReadError> {
    if src.len() > MAX_INPUT_BYTES {
        return Err(ReadError::TooLarge { bytes: src.len() });
    }
    let mut cursor = Cursor::new(src);
    let mut file = XschemFile {
        strings: String::with_capacity(src.len()),
        ..XschemFile::default()
    };
    let arena_capacity = file.strings.capacity();
    read_level(&mut cursor, &mut file, Level::Top);
    // Brace contents never exceed the source, so the top arena never reallocated.
    assert_eq!(file.strings.capacity(), arena_capacity);
    assert_eq!(cursor.pos, src.len(), "the reader consumes all input");
    assert!(file.strings.len() <= src.len());
    Ok(file)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Top,
    Embedded,
    /// A nested or stray block whose records are read only to find its end.
    Skipped,
}

#[derive(PartialEq, Eq)]
enum LevelEnd {
    EndOfInput,
    Closed,
}

/// Reads records into `out` until end of input or, inside a block, the closing `]`.
fn read_level(c: &mut Cursor, out: &mut XschemFile, level: Level) -> LevelEnd {
    assert!(out.records_read == 0 && out.issues.is_empty());
    let mut prev_instance: Option<u32> = None;
    let end = loop {
        c.skip_ws();
        if c.at_end() {
            break LevelEnd::EndOfInput;
        }
        let start = c.pos;
        let tag = c.bytes[start];
        c.pos += 1;
        if tag == b']' && level != Level::Top {
            c.skip_line();
            break LevelEnd::Closed;
        }
        let (line, col) = c.loc(start);
        if out.records_read == MAX_RECORDS {
            out.issues.push(Issue {
                line,
                col,
                tag,
                kind: IssueKind::TooManyRecords,
            });
            c.pos = c.bytes.len();
            out.records_read += 1;
            break LevelEnd::EndOfInput;
        }
        out.records_read += 1;
        let instance_before = prev_instance.take();
        let result = match tag {
            b'v' | b'G' | b'K' | b'V' | b'S' | b'E' => read_global(c, out, tag, line),
            b'#' | b'F' => {
                c.skip_line();
                out.comments += 1;
                Ok(())
            }
            b'L' | b'B' => read_quad(c, out, tag, line),
            b'P' => read_polygon(c, out, line),
            b'A' => read_arc(c, out, line),
            b'T' => read_text(c, out, line),
            b'N' => read_wire(c, out, line),
            b'C' => {
                let r = read_instance(c, out, line);
                if r.is_ok() {
                    prev_instance = Some(out.instances.symref.len() as u32 - 1);
                }
                r
            }
            b'[' => {
                c.skip_line();
                match (level, instance_before) {
                    (Level::Top, Some(instance)) => {
                        let mut body = XschemFile::default();
                        if read_level(c, &mut body, Level::Embedded) == LevelEnd::Closed {
                            out.embedded.push(Embedded {
                                instance,
                                line,
                                body,
                            });
                            Ok(())
                        } else {
                            Err(IssueKind::UnterminatedEmbed)
                        }
                    }
                    (Level::Top, None) => skip_block(c, IssueKind::StrayEmbedOpen),
                    (Level::Embedded | Level::Skipped, _) => skip_block(c, IssueKind::NestedEmbed),
                }
            }
            b']' => {
                c.skip_line();
                Err(IssueKind::StrayEmbedClose)
            }
            _ => {
                c.skip_record();
                Err(IssueKind::UnknownTag)
            }
        };
        if let Err(kind) = result {
            out.issues.push(Issue {
                line,
                col,
                tag,
                kind,
            });
        }
    };
    assert_eq!(
        out.records_read,
        out.records_kept() + out.comments + out.issues.len() as u32,
        "record conservation"
    );
    end
}

/// Skips a `[ … ]` block that is not kept, reading its records so braces are honoured.
fn skip_block(c: &mut Cursor, kind: IssueKind) -> Result<(), IssueKind> {
    let mut scratch = XschemFile::default();
    match read_level(c, &mut scratch, Level::Skipped) {
        LevelEnd::Closed => Err(kind),
        LevelEnd::EndOfInput => Err(IssueKind::UnterminatedEmbed),
    }
}

/// Undo point for a record that may fail half-way: arena, points and roundings are
/// truncated back on error so a dropped record leaves nothing behind.
struct Mark {
    strings: usize,
    points: usize,
    roundings: usize,
}

impl Mark {
    fn new(out: &XschemFile) -> Mark {
        Mark {
            strings: out.strings.len(),
            points: out.points.len(),
            roundings: out.roundings.len(),
        }
    }

    fn rollback(&self, c: &mut Cursor, out: &mut XschemFile, kind: IssueKind) -> IssueKind {
        out.strings.truncate(self.strings);
        out.points.truncate(self.points);
        out.roundings.truncate(self.roundings);
        if !matches!(kind, IssueKind::MissingBrace | IssueKind::UnterminatedBrace) {
            c.skip_record();
        }
        kind
    }
}

fn read_global(c: &mut Cursor, out: &mut XschemFile, tag: u8, line: u32) -> Result<(), IssueKind> {
    let mark = Mark::new(out);
    let text = c
        .brace(&mut out.strings)
        .map_err(|k| mark.rollback(c, out, k))?;
    c.skip_line();
    let tag = match tag {
        b'v' => GlobalTag::Version,
        b'G' => GlobalTag::G,
        b'K' => GlobalTag::K,
        b'V' => GlobalTag::V,
        b'S' => GlobalTag::S,
        _ => GlobalTag::E,
    };
    out.globals.tag.push(tag);
    out.globals.text.push(text);
    out.globals.line.push(line);
    Ok(())
}

fn read_quad(c: &mut Cursor, out: &mut XschemFile, tag: u8, line: u32) -> Result<(), IssueKind> {
    let mark = Mark::new(out);
    let fields = (|| {
        let layer = c.layer()?;
        let xy = [
            c.milli(&mut out.roundings, NumField::Coord)?,
            c.milli(&mut out.roundings, NumField::Coord)?,
            c.milli(&mut out.roundings, NumField::Coord)?,
            c.milli(&mut out.roundings, NumField::Coord)?,
        ];
        let props = c.brace(&mut out.strings)?;
        Ok((layer, xy, props))
    })();
    let (layer, xy, props) = fields.map_err(|k| mark.rollback(c, out, k))?;
    c.skip_line();
    let table = if tag == b'L' {
        &mut out.lines
    } else {
        &mut out.boxes
    };
    table.layer.push(layer);
    table.xy.push(xy);
    table.props.push(props);
    table.line.push(line);
    Ok(())
}

fn read_wire(c: &mut Cursor, out: &mut XschemFile, line: u32) -> Result<(), IssueKind> {
    let mark = Mark::new(out);
    let fields = (|| {
        let xy = [
            c.milli(&mut out.roundings, NumField::Coord)?,
            c.milli(&mut out.roundings, NumField::Coord)?,
            c.milli(&mut out.roundings, NumField::Coord)?,
            c.milli(&mut out.roundings, NumField::Coord)?,
        ];
        Ok((xy, c.brace(&mut out.strings)?))
    })();
    let (xy, props) = fields.map_err(|k| mark.rollback(c, out, k))?;
    c.skip_line();
    out.wires.xy.push(xy);
    out.wires.props.push(props);
    out.wires.line.push(line);
    Ok(())
}

fn read_polygon(c: &mut Cursor, out: &mut XschemFile, line: u32) -> Result<(), IssueKind> {
    let mark = Mark::new(out);
    let first = out.points.len() as u32;
    let fields = (|| {
        let layer = c.layer()?;
        let count = c.int()?;
        if !(1..=i64::from(MAX_POLYGON_POINTS)).contains(&count) {
            return Err(IssueKind::PointCount);
        }
        for _ in 0..count {
            let p = [
                c.milli(&mut out.roundings, NumField::Coord)?,
                c.milli(&mut out.roundings, NumField::Coord)?,
            ];
            out.points.push(p);
        }
        Ok((layer, count as u32, c.brace(&mut out.strings)?))
    })();
    let (layer, count, props) = fields.map_err(|k| mark.rollback(c, out, k))?;
    c.skip_line();
    assert_eq!(out.points.len() as u32, first + count);
    out.polygons.layer.push(layer);
    out.polygons.first.push(first);
    out.polygons.count.push(count);
    out.polygons.props.push(props);
    out.polygons.line.push(line);
    Ok(())
}

fn read_arc(c: &mut Cursor, out: &mut XschemFile, line: u32) -> Result<(), IssueKind> {
    let mark = Mark::new(out);
    let fields = (|| {
        let layer = c.layer()?;
        let circle = [
            c.milli(&mut out.roundings, NumField::Coord)?,
            c.milli(&mut out.roundings, NumField::Coord)?,
            c.milli(&mut out.roundings, NumField::Coord)?,
        ];
        let angles = [
            c.milli(&mut out.roundings, NumField::Angle)?,
            c.milli(&mut out.roundings, NumField::Angle)?,
        ];
        Ok((layer, circle, angles, c.brace(&mut out.strings)?))
    })();
    let (layer, circle, angles, props) = fields.map_err(|k| mark.rollback(c, out, k))?;
    c.skip_line();
    out.arcs.layer.push(layer);
    out.arcs.circle.push(circle);
    out.arcs.angles.push(angles);
    out.arcs.props.push(props);
    out.arcs.line.push(line);
    Ok(())
}

fn read_text(c: &mut Cursor, out: &mut XschemFile, line: u32) -> Result<(), IssueKind> {
    let mark = Mark::new(out);
    let fields = (|| {
        let text = c.brace(&mut out.strings)?;
        let xy = [
            c.milli(&mut out.roundings, NumField::Coord)?,
            c.milli(&mut out.roundings, NumField::Coord)?,
        ];
        let (rot, flip) = (c.short()?, c.short()?);
        let scale = [
            c.milli(&mut out.roundings, NumField::Scale)?,
            c.milli(&mut out.roundings, NumField::Scale)?,
        ];
        Ok((text, xy, rot, flip, scale, c.brace(&mut out.strings)?))
    })();
    let (text, xy, rot, flip, scale, props) = fields.map_err(|k| mark.rollback(c, out, k))?;
    c.skip_line();
    let t = &mut out.texts;
    t.text.push(text);
    t.xy.push(xy);
    t.rot.push(rot);
    t.flip.push(flip);
    t.scale.push(scale);
    t.props.push(props);
    t.line.push(line);
    Ok(())
}

fn read_instance(c: &mut Cursor, out: &mut XschemFile, line: u32) -> Result<(), IssueKind> {
    let mark = Mark::new(out);
    let fields = (|| {
        let symref = c.brace(&mut out.strings)?;
        let xy = [
            c.milli(&mut out.roundings, NumField::Coord)?,
            c.milli(&mut out.roundings, NumField::Coord)?,
        ];
        let (rot, flip) = (c.short()?, c.short()?);
        Ok((symref, xy, rot, flip, c.brace(&mut out.strings)?))
    })();
    let (symref, xy, rot, flip, props) = fields.map_err(|k| mark.rollback(c, out, k))?;
    c.skip_line();
    let t = &mut out.instances;
    t.symref.push(symref);
    t.xy.push(xy);
    t.rot.push(rot);
    t.flip.push(flip);
    t.props.push(props);
    t.line.push(line);
    Ok(())
}

/// Byte cursor with lazy, monotonic line/column bookkeeping.
struct Cursor<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// Line number at `counted` (1-based) and the offset where that line starts.
    line: u32,
    line_start: usize,
    counted: usize,
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

impl<'a> Cursor<'a> {
    fn new(src: &'a str) -> Cursor<'a> {
        Cursor {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            line: 1,
            line_start: 0,
            counted: 0,
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    /// Returns the 1-based (line, byte column) of offset `at`.
    fn loc(&mut self, at: usize) -> (u32, u32) {
        assert!(at <= self.bytes.len());
        if at < self.counted {
            (self.line, self.line_start, self.counted) = (1, 0, 0);
        }
        for (i, b) in self.bytes[self.counted..at].iter().enumerate() {
            if *b == b'\n' {
                self.line += 1;
                self.line_start = self.counted + i + 1;
            }
        }
        self.counted = at;
        (self.line, (at - self.line_start + 1) as u32)
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && is_space(self.bytes[self.pos]) {
            self.pos += 1;
        }
    }

    /// Skips to just past the next `\n` (or to the end).
    fn skip_line(&mut self) {
        match self.bytes[self.pos..].iter().position(|b| *b == b'\n') {
            Some(n) => self.pos += n + 1,
            None => self.pos = self.bytes.len(),
        }
    }

    /// Skips the rest of a record: to the end of the line, jumping over brace strings
    /// (which may span lines) on the way.
    fn skip_record(&mut self) {
        let mut sink = String::new();
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'\n' => {
                    self.pos += 1;
                    return;
                }
                b'{' => {
                    sink.clear();
                    if self.brace(&mut sink).is_err() {
                        return;
                    }
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Returns the next whitespace-delimited numeric token (stops before `{` and `}`).
    fn token(&mut self) -> Result<(usize, &'a str), IssueKind> {
        assert!(self.pos <= self.bytes.len());
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if is_space(b) || b == b'{' || b == b'}' {
                break;
            }
            self.pos += 1;
        }
        if start == self.pos {
            return Err(IssueKind::MissingNumber);
        }
        // Token bounds sit on ASCII bytes or the ends, so they are char boundaries.
        Ok((start, &self.src[start..self.pos]))
    }

    fn int(&mut self) -> Result<i64, IssueKind> {
        let (_, t) = self.token()?;
        let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(IssueKind::BadNumber);
        }
        t.parse::<i64>().map_err(|_| IssueKind::NumberOutOfRange)
    }

    fn layer(&mut self) -> Result<u8, IssueKind> {
        u8::try_from(self.int()?).map_err(|_| IssueKind::LayerOutOfRange)
    }

    fn short(&mut self) -> Result<i16, IssueKind> {
        i16::try_from(self.int()?).map_err(|_| IssueKind::NumberOutOfRange)
    }

    /// Reads a coordinate-like number as milli-units, logging inexact values.
    fn milli(&mut self, roundings: &mut Vec<Rounding>, field: NumField) -> Result<i32, IssueKind> {
        let (start, t) = self.token()?;
        let s = decimal::parse_scaled(t, 3).map_err(|e| match e {
            DecimalError::Syntax => IssueKind::BadNumber,
            DecimalError::Overflow => IssueKind::NumberOutOfRange,
        })?;
        let milli = i32::try_from(s.value).map_err(|_| IssueKind::NumberOutOfRange)?;
        if !s.exact {
            let (line, col) = self.loc(start);
            roundings.push(Rounding {
                line,
                col,
                field,
                milli,
                error_micro: s.error_milli,
            });
        }
        Ok(milli)
    }

    /// Reads a brace string into `arena`, removing file-level escapes and `\r`.
    fn brace(&mut self, arena: &mut String) -> Result<Span, IssueKind> {
        let b = self.bytes;
        match b[self.pos..].iter().position(|x| *x == b'{') {
            Some(n) => self.pos += n + 1,
            None => {
                self.pos = b.len();
                return Err(IssueKind::MissingBrace);
            }
        }
        let start = arena.len();
        let open = self.pos;
        let mut seg = self.pos;
        loop {
            if self.pos >= b.len() {
                // A trailing `\` steps past the end; park the cursor on it.
                self.pos = b.len();
                arena.truncate(start);
                return Err(IssueKind::UnterminatedBrace);
            }
            match b[self.pos] {
                b'}' => {
                    arena.push_str(&self.src[seg..self.pos]);
                    self.pos += 1;
                    break;
                }
                b'\r' => {
                    arena.push_str(&self.src[seg..self.pos]);
                    self.pos += 1;
                    seg = self.pos;
                }
                b'\\' => {
                    arena.push_str(&self.src[seg..self.pos]);
                    self.pos += 1;
                    while self.pos < b.len() && b[self.pos] == b'\r' {
                        self.pos += 1;
                    }
                    // The escaped char starts the next copied segment, whatever it is.
                    seg = self.pos;
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
        assert!(
            arena.len() - start < self.pos - open + 1,
            "unescaping never grows text"
        );
        Ok(Span::new(start, arena.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotated_symbol_example() {
        // xschem-feature-inventory.md §5.15, ipin.sym (header trimmed).
        let src = "v {xschem version=3.4.4 file_version=1.2\n* license\n}\nG {}\nK {type=ipin\nformat=\"*.ipin @lab\"\ntemplate=\"name=p1 lab=xxx\"\n}\nV {}\nS {}\nE {}\nL 5 -5 0 0 0 {}\nB 5 -1.25 -1.25 1.25 1.25 {name=p dir=out}\nP 5 7 -5 0 -8.75 -5 -17.5 -5 -13.75 0 -17.5 5 -8.75 5 -5 0 {fill=true}\nT {@lab} -18.75 -8.75 0 1 0.33 0.33 {}\nT {@spice_get_voltage} 1.875 3.90625 0 0 0.2 0.2 {layer=15}\n";
        let f = read(src).unwrap();
        assert!(f.issues.is_empty(), "{:?}", f.issues);
        assert_eq!(f.globals.tag.len(), 6);
        assert_eq!(
            f.global(GlobalTag::Version),
            Some("xschem version=3.4.4 file_version=1.2\n* license\n")
        );
        assert_eq!(
            f.global(GlobalTag::K),
            Some("type=ipin\nformat=\"*.ipin @lab\"\ntemplate=\"name=p1 lab=xxx\"\n")
        );
        assert_eq!(f.boxes.xy[0], [-1250, -1250, 1250, 1250]);
        assert_eq!((f.polygons.count[0], f.points[1]), (7, [-8750, -5000]));
        assert_eq!(
            (f.texts.rot[0], f.texts.flip[0], f.texts.scale[0]),
            (0, 1, [330, 330])
        );
        // 3.90625 → 3906.25 milli: the one inexact value in the file.
        assert_eq!(
            f.roundings,
            vec![Rounding {
                line: 16,
                col: 30,
                field: NumField::Coord,
                milli: 3906,
                error_micro: 250
            }]
        );
        assert_eq!(f.records_read, 11);
    }

    #[test]
    fn brace_escapes_and_literal_open_brace() {
        let f = read("K {a\\}b\\\\c{d\r\n}\n").unwrap();
        assert_eq!(f.global(GlobalTag::K), Some("a}b\\c{d\n"));
        // Everything before the first `{` is skipped, even other text.
        let f = read("S junk {x}\n").unwrap();
        assert_eq!(f.global(GlobalTag::S), Some("x"));
    }

    #[test]
    fn embedded_symbol_binds_to_previous_instance() {
        let src = "C {title.sym} 160 -30 0 0 {name=l2 embed=true}\n[\nv {xschem version=3.0.0 file_version=1.2}\nG {type=logo}\nL 6 225 0 1020 0 {}\n[\nL 1 0 0 1 1 {}\n]\n]\nC {x.sym} 0 0 1 0 {name=x}\n";
        let f = read(src).unwrap();
        assert_eq!(f.instances.symref.len(), 2);
        assert_eq!(f.embedded.len(), 1);
        let e = &f.embedded[0];
        assert_eq!((e.instance, e.line), (0, 2));
        assert_eq!(e.body.lines.xy, vec![[225_000, 0, 1_020_000, 0]]);
        assert_eq!(e.body.issues[0].kind, IssueKind::NestedEmbed);
        assert!(f.issues.is_empty());
    }

    #[test]
    fn malformed_records_are_reported_not_kept() {
        let src = "L 4 0 0 x 1 {}\nB 300 0 0 1 1 {}\nP 4 0 {}\nQ {junk\n}\n[\n]\n]\nL 4 1e20 0 0 0 {}\nN 1 2 3 4 {ok}\nT {unterminated";
        let f = read(src).unwrap();
        let kinds: Vec<_> = f.issues.iter().map(|i| (i.line, i.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                (1, IssueKind::BadNumber),
                (2, IssueKind::LayerOutOfRange),
                (3, IssueKind::PointCount),
                (4, IssueKind::UnknownTag),
                (6, IssueKind::StrayEmbedOpen),
                (8, IssueKind::StrayEmbedClose),
                (9, IssueKind::NumberOutOfRange),
                (11, IssueKind::UnterminatedBrace),
            ]
        );
        assert_eq!(f.wires.xy, vec![[1000, 2000, 3000, 4000]]);
        assert!(
            f.strings == "ok",
            "dropped records leave nothing in the arena: {:?}",
            f.strings
        );
    }

    #[test]
    fn rejects_oversized_input_only() {
        assert!(read("").unwrap().records_read == 0);
        assert!(read("\u{0}\n\u{1}").unwrap().issues.len() == 2);
    }
}
