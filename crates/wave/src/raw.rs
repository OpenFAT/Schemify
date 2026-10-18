//! `.raw` reader: ngspice, Xyce and LTspice; ASCII (`Values:`) and binary (`Binary:`).
//!
//! Bytes in, `Vec<RawPlot>` out (one per analysis block, in file order). Flavours handled:
//!
//! - ngspice binary: row-major little-endian f64 (pairs for complex); ASCII: one value per line,
//!   `%.15e`, complex as `re,im`.
//! - Xyce (`-r`, `-r … -a`): same layout; ASCII has 9 significant digits and writes complex as
//!   `re, im` (space after the comma); node names are bare (`OUT`, `V1#branch`); each `.step` is a
//!   separate block (see [`crate::group_datasets`]).
//! - LTspice binary: UTF-16LE header, f64 scale with f32 data (or all f64), `fastaccess`
//!   column-major, negative times marking compressed points (|t| is used).
//!
//! The reader is total: any byte string yields `Ok` or a [`RawError`], never a panic, and no
//! allocation exceeds what the input's size can fill (a header claiming 2^32 points in a
//! 100-byte file fails before allocating).

use std::borrow::Cow;

use crate::data::{RawPlot, VarKind, Variable};

/// Why a `.raw` file could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum RawError {
    /// No bytes at all.
    Empty,
    /// A header field is missing, malformed or inconsistent.
    Header(String),
    /// The data section is shorter than the header's point × variable count requires.
    Truncated {
        /// Bytes the header implies.
        expected: usize,
        /// Bytes present.
        found: usize,
    },
    /// An ASCII value (or point index) is missing or not a number.
    Values(u32),
    /// Only blank lines.
    NoPlots,
}

impl std::fmt::Display for RawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RawError::Empty => f.write_str("empty file"),
            RawError::Header(m) => write!(f, "malformed header: {m}"),
            RawError::Truncated { expected, found } => {
                write!(
                    f,
                    "data truncated: expected {expected} bytes, found {found}"
                )
            }
            RawError::Values(p) => write!(f, "malformed ascii values at point {p}"),
            RawError::NoPlots => f.write_str("no plots found in file"),
        }
    }
}

impl std::error::Error for RawError {}

/// Parses a complete `.raw` file into one [`RawPlot`] per analysis block.
///
/// # Errors
///
/// See [`RawError`]; the first malformed block fails the whole file.
///
/// # Time complexity
///
/// O(bytes). Binary data is transposed to column-major once.
pub fn parse_raw(bytes: &[u8]) -> Result<Vec<RawPlot>, RawError> {
    if bytes.is_empty() {
        return Err(RawError::Empty);
    }
    let mut rd = Reader::new(bytes);
    let mut plots = Vec::new();
    // Bounded: every iteration consumes at least one header line or ends the loop.
    while !rd.at_end() {
        let before = rd.pos;
        match parse_one_plot(&mut rd)? {
            Some(p) => plots.push(p),
            None => break, // trailing blank lines
        }
        assert!(rd.pos > before, "parser made no progress");
    }
    if plots.is_empty() {
        return Err(RawError::NoPlots);
    }
    debug_assert!(plots.iter().all(|p| !p.steps().is_empty()));
    Ok(plots)
}

#[derive(Default)]
struct Header {
    plotname: String,
    command: String,
    flags: String,
    n_vars: u32,
    n_points: u32,
    variables: Vec<Variable>,
    binary: bool,
}

fn parse_one_plot(rd: &mut Reader) -> Result<Option<RawPlot>, RawError> {
    let Some(h) = parse_header(rd)? else {
        return Ok(None);
    };
    let flags = h.flags.to_ascii_lowercase();
    let complex = flags.contains("complex");
    let fastaccess = flags.contains("fastaccess");
    let command = h.command.to_ascii_lowercase();
    let ltspice = command.contains("ltspice") || command.contains("linear technology");
    let n = h.n_points as usize;

    let (mut re, im) = if h.binary {
        parse_binary(rd, &h, complex, fastaccess, ltspice)?
    } else {
        parse_ascii_values(rd, &h, complex)?
    };
    // LTspice marks compressed transient points with a negative time; consumers take |t|.
    if ltspice && h.variables[0].kind == VarKind::Time {
        for x in &mut re[..n] {
            *x = x.abs();
        }
    }
    let plot = RawPlot::from_columns(&h.plotname, h.variables, re, im)?;
    Ok(Some(plot))
}

fn parse_header(rd: &mut Reader) -> Result<Option<Header>, RawError> {
    let mut h = Header::default();
    let mut seen_any = false;
    loop {
        let Some(line) = rd.read_line() else {
            return if seen_any {
                Err(RawError::Header("unexpected end of file in header".into()))
            } else {
                Ok(None)
            };
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        seen_any = true;
        if let Some(v) = value_of(trimmed, "plotname:") {
            h.plotname = v.to_owned();
        } else if let Some(v) = value_of(trimmed, "command:") {
            h.command = v.to_owned();
        } else if let Some(v) = value_of(trimmed, "flags:") {
            h.flags = v.to_owned();
        } else if let Some(v) = value_of(trimmed, "no. variables:") {
            h.n_vars = v
                .parse()
                .map_err(|_| RawError::Header(format!("bad variable count: {v}")))?;
        } else if let Some(v) = value_of(trimmed, "no. points:") {
            h.n_points = v
                .parse()
                .map_err(|_| RawError::Header(format!("bad point count: {v}")))?;
        } else if let Some(rest) = value_of(trimmed, "variables:") {
            if h.n_vars == 0 {
                return Err(RawError::Header("Variables: before No. Variables:".into()));
            }
            // Some writers put the first entry on the `Variables:` line itself.
            if !rest.is_empty() {
                h.variables.push(parse_var_line(rest)?);
            }
            // Bounded by n_vars, and each pass consumes a line or fails.
            while h.variables.len() < h.n_vars as usize {
                let vl = rd
                    .read_line()
                    .ok_or_else(|| RawError::Header("end of file in variable list".into()))?;
                h.variables.push(parse_var_line(&vl)?);
            }
        } else if value_of(trimmed, "values:").is_some() {
            h.binary = false;
            break;
        } else if value_of(trimmed, "binary:").is_some() {
            h.binary = true;
            break;
        }
        // Unknown header lines (Title:, Date:, Offset:, Dimensions:, …) are skipped.
    }
    if h.n_points == 0 || h.n_vars == 0 {
        return Err(RawError::Header("zero points or variables".into()));
    }
    if h.variables.len() != h.n_vars as usize {
        return Err(RawError::Header(format!(
            "variable count mismatch: declared {}, listed {}",
            h.n_vars,
            h.variables.len()
        )));
    }
    Ok(Some(h))
}

/// `"<idx> <name> <type> [params…]"`.
fn parse_var_line(line: &str) -> Result<Variable, RawError> {
    let mut it = line.split_whitespace();
    let _idx = it.next();
    let name = it
        .next()
        .ok_or_else(|| RawError::Header(format!("bad variable line: {line}")))?;
    let kind = it.next().map_or(VarKind::Other, VarKind::from_type_str);
    Ok(Variable {
        name: name.to_owned(),
        kind,
    })
}

/// Case-insensitive `"Key:" value` extractor; byte-wise, so a multibyte character in the line
/// never splits.
fn value_of<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let b = line.as_bytes();
    if b.len() >= key.len() && b[..key.len()].eq_ignore_ascii_case(key.as_bytes()) {
        // `key` is ASCII and matched, so `key.len()` is a char boundary.
        Some(line[key.len()..].trim())
    } else {
        None
    }
}

/// Parses the `Values:` section as a token stream: per point, the index then one value per
/// variable (two numbers for complex). Commas and whitespace both separate, so ngspice's
/// `re,im` and Xyce's `re, im` read the same; line breaks carry no meaning.
fn parse_ascii_values(
    rd: &mut Reader,
    h: &Header,
    complex: bool,
) -> Result<(Vec<f64>, Vec<f64>), RawError> {
    let n = h.n_points as usize;
    let nv = h.n_vars as usize;
    let parts = if complex { 2 } else { 1 };
    // Every number needs at least two units (a digit and a separator): refuse before allocating
    // what the input cannot fill.
    let need = n
        .checked_mul(nv)
        .and_then(|c| c.checked_mul(parts + 1))
        .and_then(|c| c.checked_mul(rd.unit))
        .ok_or_else(|| RawError::Header("point × variable count overflows".into()))?;
    if need > rd.remaining() {
        return Err(RawError::Truncated {
            expected: need,
            found: rd.remaining(),
        });
    }
    let mut re = vec![0.0f64; nv * n];
    let mut im = if complex {
        vec![0.0f64; nv * n]
    } else {
        Vec::new()
    };
    for point in 0..n {
        let bad = || RawError::Values(point as u32);
        let idx = rd.number().ok_or_else(bad)?;
        if idx != point as f64 {
            return Err(bad());
        }
        for v in 0..nv {
            re[v * n + point] = rd.number().ok_or_else(bad)?;
            if complex {
                im[v * n + point] = rd.number().ok_or_else(bad)?;
            }
        }
    }
    rd.skip_line();
    Ok((re, im))
}

/// Binary layouts, all little-endian:
///   - ngspice / Xyce real: row-major, every value f64
///   - ngspice / Xyce / LTspice complex: row-major, every value (f64, f64)
///   - LTspice real: row-major, scale f64, data f32 (or all f64, detected by size)
///   - LTspice fastaccess: column-major, scale f64 column, then f32 data columns
fn parse_binary(
    rd: &mut Reader,
    h: &Header,
    complex: bool,
    fastaccess: bool,
    ltspice: bool,
) -> Result<(Vec<f64>, Vec<f64>), RawError> {
    let n = h.n_points as usize;
    let nv = h.n_vars as usize;
    let overflow = || RawError::Header("point × variable count overflows".into());
    let cells = n.checked_mul(nv).ok_or_else(overflow)?;

    if complex {
        let data = rd.take_bytes(cells.checked_mul(16).ok_or_else(overflow)?)?;
        let mut re = vec![0.0f64; cells];
        let mut im = vec![0.0f64; cells];
        transpose(data, 16, 0, n, nv, &mut re);
        transpose(data, 16, 8, n, nv, &mut im);
        return Ok((re, im));
    }

    let all_f64 = cells.checked_mul(8).ok_or_else(overflow)?;
    let row_mixed = (nv - 1)
        .checked_mul(4)
        .and_then(|b| b.checked_add(8))
        .ok_or_else(overflow)?;
    let mixed = n.checked_mul(row_mixed).ok_or_else(overflow)?;
    let avail = rd.remaining();
    if ltspice && avail >= mixed && (avail < all_f64 || nv == 1) {
        let data = rd.take_bytes(mixed)?;
        let mut re = vec![0.0f64; cells];
        if fastaccess {
            let (scale, rest) = data.split_at(n * 8);
            for (p, b) in scale.chunks_exact(8).enumerate() {
                re[p] = f64_le(b);
            }
            for (k, b) in rest.chunks_exact(4).enumerate() {
                re[n + k] = f64::from(f32_le(b));
            }
        } else {
            for (p, row) in data.chunks_exact(row_mixed).enumerate() {
                re[p] = f64_le(&row[..8]);
                for (v, b) in row[8..].chunks_exact(4).enumerate() {
                    re[(v + 1) * n + p] = f64::from(f32_le(b));
                }
            }
        }
        return Ok((re, Vec::new()));
    }
    let data = rd.take_bytes(all_f64)?;
    let mut re = vec![0.0f64; cells];
    transpose(data, 8, 0, n, nv, &mut re);
    Ok((re, Vec::new()))
}

/// Points per block of [`transpose`]: one block writes 16 consecutive `f64` (two cache lines)
/// of each column, where a point at a time would touch a line of every column per point.
const BLOCK: usize = 16;
/// Widest row [`transpose`] copies a point at a time: up to here the per-point column writes
/// stay in cache and that loop is the faster one; above it, blocked (measured by `bench_parse`:
/// 40k × 256 1.8× faster blocked, 10k × 1024 3.4×; 1M × 4 a little slower blocked).
const POINTWISE_NV_MAX: usize = 128;

/// Transposes `n` row-major rows of `nv` cells of `cell` bytes into columns: `out[v * n + p]`
/// is the little-endian `f64` at byte `off` of cell `(p, v)`. Rows wider than
/// [`POINTWISE_NV_MAX`] go [`BLOCK`] points at a time, so a block's reads stay in cache and
/// each column write fills whole lines.
fn transpose(data: &[u8], cell: usize, off: usize, n: usize, nv: usize, out: &mut [f64]) {
    assert_eq!(data.len(), n * nv * cell, "whole rows");
    assert_eq!(out.len(), n * nv, "one value per cell");
    assert!(off + 8 <= cell, "the value is inside its cell");
    if n == 0 {
        return;
    }
    let row = nv * cell;
    if nv <= POINTWISE_NV_MAX {
        for (p, r) in data.chunks_exact(row).enumerate() {
            for (v, c) in r.chunks_exact(cell).enumerate() {
                out[v * n + p] = f64_le(&c[off..off + 8]);
            }
        }
        return;
    }
    for p0 in (0..n).step_by(BLOCK) {
        let rows = &data[p0 * row..(p0 + BLOCK).min(n) * row];
        let b = rows.len() / row;
        debug_assert!((1..=BLOCK).contains(&b));
        for (v, col) in out.chunks_exact_mut(n).enumerate() {
            let at = v * cell + off;
            for (x, r) in col[p0..p0 + b].iter_mut().zip(rows.chunks_exact(row)) {
                *x = f64_le(&r[at..at + 8]);
            }
        }
    }
}

/// Token separator of the `Values:` section: ASCII whitespace or `,`.
#[inline]
fn is_sep(u: u8) -> bool {
    u == b',' || u == b' ' || (0x09..=0x0d).contains(&u)
}

#[inline]
fn f64_le(b: &[u8]) -> f64 {
    f64::from_le_bytes(b[..8].try_into().expect("8 bytes"))
}

#[inline]
fn f32_le(b: &[u8]) -> f32 {
    f32::from_le_bytes(b[..4].try_into().expect("4 bytes"))
}

/// Line- and token-oriented cursor over UTF-8 or UTF-16LE text, with raw byte access for binary
/// sections. `unit` is the code-unit width (1 or 2 bytes).
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    unit: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        let bom = bytes.len() >= 2 && bytes[..2] == [0xFF, 0xFE];
        // UTF-16LE: a BOM, or an ASCII first character with a zero high byte.
        let utf16 = bom || (bytes.len() >= 2 && bytes[1] == 0);
        Reader {
            bytes,
            pos: if bom { 2 } else { 0 },
            unit: if utf16 { 2 } else { 1 },
        }
    }

    fn at_end(&self) -> bool {
        self.pos + self.unit > self.bytes.len()
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos.min(self.bytes.len())
    }

    /// Code unit at the cursor (a byte, or a UTF-16 unit), if a whole one remains.
    fn peek(&self) -> Option<u32> {
        if self.at_end() {
            return None;
        }
        Some(if self.unit == 1 {
            u32::from(self.bytes[self.pos])
        } else {
            u32::from(u16::from_le_bytes([
                self.bytes[self.pos],
                self.bytes[self.pos + 1],
            ]))
        })
    }

    /// One text line without its `\r\n`; `None` at end of input. Always consumes at least one
    /// unit, and a trailing partial UTF-16 unit is dropped.
    fn read_line(&mut self) -> Option<Cow<'a, str>> {
        if self.at_end() {
            self.pos = self.bytes.len();
            return None;
        }
        if self.unit == 1 {
            let rest = &self.bytes[self.pos..];
            let len = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
            self.pos += (len + 1).min(rest.len());
            let line = &rest[..len];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            return Some(String::from_utf8_lossy(line));
        }
        let mut out = String::new();
        while let Some(u) = self.peek() {
            self.pos += 2;
            if u == u32::from(b'\n') {
                break;
            }
            if u != u32::from(b'\r') {
                // .raw headers are ASCII in practice; anything else is shown lossily.
                out.push(char::from_u32(u).unwrap_or('\u{FFFD}'));
            }
        }
        if self.at_end() {
            self.pos = self.bytes.len();
        }
        Some(Cow::Owned(out))
    }

    /// Skips the rest of the current line (after the last ASCII value).
    fn skip_line(&mut self) {
        while let Some(u) = self.peek() {
            self.pos += self.unit;
            if u == u32::from(b'\n') {
                break;
            }
        }
    }

    /// Next number token: separators are ASCII whitespace and `,`. `None` at end of input, for a
    /// token over 63 units, or one that is not a number.
    fn number(&mut self) -> Option<f64> {
        // ponytail: one unit at a time for both encodings. `str::parse::<f64>` dominates (a byte
        // slicing fast path measured no faster on 8 MB of ngspice ASCII).
        while let Some(u) = self.peek() {
            if !(u < 0x80 && is_sep(u as u8)) {
                break;
            }
            self.pos += self.unit;
        }
        let mut buf = [0u8; 64];
        let mut len = 0;
        while let Some(u) = self.peek() {
            if u < 0x80 && is_sep(u as u8) {
                break;
            }
            if len == buf.len() || u > 0x7f {
                return None;
            }
            buf[len] = u as u8;
            len += 1;
            self.pos += self.unit;
        }
        if len == 0 {
            return None;
        }
        std::str::from_utf8(&buf[..len]).ok()?.parse().ok()
    }

    /// Consumes exactly `want` raw bytes.
    fn take_bytes(&mut self, want: usize) -> Result<&'a [u8], RawError> {
        let avail = self.remaining();
        if avail < want {
            return Err(RawError::Truncated {
                expected: want,
                found: avail,
            });
        }
        let out = &self.bytes[self.pos..self.pos + want];
        self.pos += want;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ngspice_ascii() -> Vec<u8> {
        b"Title: rc lowpass
Date: today
Plotname: Transient Analysis
Flags: real
No. Variables: 3
No. Points: 4
Variables:
\t0\ttime\ttime
\t1\tv(out)\tvoltage
\t2\ti(vdd)\tcurrent
Values:
0\t0.0
\t0.0
\t1e-3
1\t1e-9
\t0.5
\t2e-3
2\t2e-9
\t0.8
\t3e-3
3\t3e-9
\t0.95
\t4e-3
"
        .to_vec()
    }

    #[test]
    fn ascii_roundtrip() {
        let plots = parse_raw(&ngspice_ascii()).unwrap();
        assert_eq!(plots.len(), 1);
        let p = &plots[0];
        assert_eq!(p.len(), 4);
        assert_eq!(p.vars().len(), 3);
        assert_eq!(p.vars()[1].name, "v(out)");
        assert_eq!(p.vars()[1].kind, VarKind::Voltage);
        assert_eq!(p.scale(), &[0.0, 1e-9, 2e-9, 3e-9]);
        assert_eq!(p.col(1), &[0.0, 0.5, 0.8, 0.95]);
        assert_eq!(p.col(2), &[1e-3, 2e-3, 3e-3, 4e-3]);
        assert_eq!(p.steps().len(), 1);
        assert_eq!(p.steps()[0], 0..4);
        assert_eq!(p.find_var("V(OUT)"), Some(1));
    }

    fn ngspice_binary(values: &[[f64; 2]]) -> Vec<u8> {
        let mut f = format!(
            "Title: t\nPlotname: Transient Analysis\nFlags: real\nCommand: version ngspice-44\nNo. Variables: 2\nNo. Points: {}\nVariables:\n\t0\ttime\ttime\n\t1\tv(out)\tvoltage\nBinary:\n",
            values.len()
        )
        .into_bytes();
        for row in values {
            f.extend_from_slice(&row[0].to_le_bytes());
            f.extend_from_slice(&row[1].to_le_bytes());
        }
        f
    }

    #[test]
    fn binary_ngspice() {
        let rows = [[0.0, 1.0], [1e-9, 2.0], [2e-9, 3.0]];
        let p = &parse_raw(&ngspice_binary(&rows)).unwrap()[0];
        assert_eq!(p.scale(), &[0.0, 1e-9, 2e-9]);
        assert_eq!(p.col(1), &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn binary_ltspice_utf16_f32_abs_time() {
        let header = "Title: * tb\nDate: x\nPlotname: Transient Analysis\nFlags: real forward\nNo. Variables: 2\nNo. Points: 3\nOffset: 0\nCommand: Linear Technology LTspice XVII\nVariables:\n\t0\ttime\ttime\n\t1\tV(out)\tvoltage\nBinary:\n";
        let mut f: Vec<u8> = Vec::new();
        for c in header.encode_utf16() {
            f.extend_from_slice(&c.to_le_bytes());
        }
        for (t, v) in [(0.0f64, 0.1f32), (-1e-9, 0.2), (2e-9, 0.3)] {
            f.extend_from_slice(&t.to_le_bytes());
            f.extend_from_slice(&v.to_le_bytes());
        }
        let p = &parse_raw(&f).unwrap()[0];
        assert_eq!(p.scale(), &[0.0, 1e-9, 2e-9]);
        assert!((p.col(1)[0] - 0.1).abs() < 1e-6 && (p.col(1)[2] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn complex_ascii_both_comma_styles() {
        for sep in [",", ", "] {
            let f = format!(
                "Title: ac\nPlotname: AC Analysis\nFlags: complex\nNo. Variables: 2\nNo. Points: 2\nVariables:\n\t0\tfrequency\tfrequency\n\t1\tv(out)\tvoltage\nValues:\n0\t1.0{sep}0.0\n\t0.5{sep}-0.5\n1\t10.0{sep}0.0\n\t0.1{sep}-0.2\n"
            );
            let p = &parse_raw(f.as_bytes()).unwrap()[0];
            assert!(p.is_complex());
            assert_eq!(p.scale(), &[1.0, 10.0]);
            assert_eq!(p.col(1), &[0.5, 0.1]);
            assert_eq!(p.col_im(1), &[-0.5, -0.2]);
        }
    }

    #[test]
    fn sweep_steps_detected() {
        let rows = [
            [0.0, 1.0],
            [1e-9, 2.0],
            [2e-9, 3.0],
            [0.0, 10.0],
            [1e-9, 20.0],
            [2e-9, 30.0],
        ];
        let p = &parse_raw(&ngspice_binary(&rows)).unwrap()[0];
        assert_eq!(p.steps(), &[0..3, 3..6]);
    }

    #[test]
    fn multiple_blocks_appended() {
        let mut f = ngspice_ascii();
        f.extend_from_slice(&ngspice_ascii());
        let plots = parse_raw(&f).unwrap();
        assert_eq!(plots.len(), 2);
        assert_eq!(plots[1].col(1), &[0.0, 0.5, 0.8, 0.95]);
    }

    /// The point-at-a-time transpose [`transpose`] replaced: the oracle.
    fn transpose_simple(data: &[u8], cell: usize, off: usize, n: usize, out: &mut [f64]) {
        let nv = out.len() / n.max(1);
        for (p, row) in data.chunks_exact((nv * cell).max(1)).enumerate() {
            for (v, c) in row.chunks_exact(cell).enumerate() {
                out[v * n + p] = f64_le(&c[off..]);
            }
        }
    }

    /// [`transpose`] (both paths) and the simple one agree bit for bit around the block and the
    /// path switch: no points, one vector, `n` not a multiple of [`BLOCK`], real and complex.
    #[test]
    fn blocked_transpose_matches_simple() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let sizes = [
            (0, 1),
            (1, 1),
            (17, 1),
            (33, 3),
            (0, 129),
            (1, 129),
            (15, 129),
            (16, 200),
            (17, 300),
            (100, 129),
        ];
        for (n, nv) in sizes {
            for (cell, off) in [(8, 0), (16, 0), (16, 8)] {
                let data: Vec<u8> = (0..n * nv * cell)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        seed as u8
                    })
                    .collect();
                let mut out = vec![0.0; n * nv];
                transpose(&data, cell, off, n, nv, &mut out);
                let mut want = vec![0.0; n * nv];
                transpose_simple(&data, cell, off, n, &mut want);
                let bits = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
                assert_eq!(
                    bits(&out),
                    bits(&want),
                    "n {n} nv {nv} cell {cell} off {off}"
                );
            }
        }
    }

    /// `cargo test --release -p schemify-wave -- --ignored --nocapture bench_parse`
    #[test]
    #[ignore = "release-mode benchmark; run explicitly"]
    fn bench_parse() {
        use std::hint::black_box;
        use std::time::Instant;
        let (n, nv) = (1_000_000usize, 4usize);
        let mut head = format!(
            "Title: t\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: {nv}\n\
             No. Points: {n}\nVariables:\n 0 time time\n 1 v(a) voltage\n 2 v(b) voltage\n\
             3 i(v1) current\n"
        );
        let mut bin = head.clone().into_bytes();
        bin.extend_from_slice(b"Binary:\n");
        head.push_str("Values:\n");
        let mut ascii = head.into_bytes();
        for p in 0..n {
            let row = [
                p as f64 * 1e-9,
                (p as f64).sin(),
                (p as f64).cos(),
                1e-3 / (1.0 + p as f64),
            ];
            for v in row {
                bin.extend_from_slice(&v.to_le_bytes());
            }
            if p < n / 10 {
                ascii.extend_from_slice(format!("{p}\t{:.15e}\n", row[0]).as_bytes());
                for v in &row[1..] {
                    ascii.extend_from_slice(format!("\t{v:.15e}\n").as_bytes());
                }
            }
        }
        let fix = ascii.windows(9).position(|w| w == b"Points: 1").unwrap();
        ascii.splice(fix + 8..fix + 15, b"100000".iter().copied());
        let time = |b: &[u8], reps: usize| {
            let mut best = f64::INFINITY;
            for _ in 0..reps {
                let t = Instant::now();
                black_box(parse_raw(black_box(b)).unwrap());
                best = best.min(t.elapsed().as_secs_f64() * 1e3);
            }
            best
        };
        let (tb, ta) = (time(&bin, 10), time(&ascii, 5));
        println!(
            "parse_raw: binary 1M x {nv} ({} MB) {tb:.1} ms; ascii 100k x {nv} ({} MB) {ta:.1} ms",
            bin.len() >> 20,
            ascii.len() >> 20
        );
        drop((bin, ascii));
        // Wide: hundreds of vectors (a large circuit's node voltages and device currents).
        let (n, nv) = (40_000usize, 256usize);
        let mut wide = format!(
            "Title: t\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: {nv}\n\
             No. Points: {n}\nVariables:\n 0 time time\n"
        );
        for v in 1..nv {
            wide.push_str(&format!(" {v} v(n{v}) voltage\n"));
        }
        wide.push_str("Binary:\n");
        let mut wide = wide.into_bytes();
        for p in 0..n {
            wide.extend_from_slice(&(p as f64 * 1e-9).to_le_bytes());
            for v in 1..nv {
                wide.extend_from_slice(&((p * nv + v) as f64).to_le_bytes());
            }
        }
        let tw = time(&wide, 10);
        println!(
            "parse_raw: binary {n} x {nv} ({} MB) {tw:.1} ms",
            wide.len() >> 20
        );
        drop(wide);
        // The transpose alone, blocked against point at a time.
        let best = |f: &mut dyn FnMut()| {
            (0..10)
                .map(|_| {
                    let t = Instant::now();
                    f();
                    t.elapsed().as_secs_f64() * 1e3
                })
                .fold(f64::INFINITY, f64::min)
        };
        for (n, nv) in [
            (1_000_000usize, 4usize),
            (500_000, 8),
            (250_000, 16),
            (125_000, 32),
            (62_500, 64),
            (31_250, 128),
            (40_000, 256),
            (10_000, 1024),
        ] {
            let payload: Vec<u8> = (0..n * nv).flat_map(|k| (k as f64).to_le_bytes()).collect();
            let mut out = vec![0.0; n * nv];
            let blocked = best(&mut || transpose(black_box(&payload), 8, 0, n, nv, &mut out));
            black_box(&out);
            let simple = best(&mut || transpose_simple(black_box(&payload), 8, 0, n, &mut out));
            black_box(&out);
            println!("transpose {n} x {nv}: blocked {blocked:.1} ms, simple {simple:.1} ms");
        }
    }

    #[test]
    fn malformed_inputs_fail_with_declared_errors() {
        let mut f = ngspice_binary(&[[0.0, 1.0], [1e-9, 2.0]]);
        f.truncate(f.len() - 4);
        assert!(matches!(parse_raw(&f), Err(RawError::Truncated { .. })));
        assert_eq!(parse_raw(b""), Err(RawError::Empty));
        assert_eq!(parse_raw(b"\n\n  \n"), Err(RawError::NoPlots));
        // A huge declared size in a tiny file fails before allocating.
        let huge = b"No. Variables: 4000000\nNo. Points: 4000000000\nVariables:\n";
        assert!(parse_raw(huge).is_err());
        let huge_bin = "No. Variables: 1\nNo. Points: 4000000000\nVariables:\n 0 t time\nBinary:\n";
        assert!(matches!(
            parse_raw(huge_bin.as_bytes()),
            Err(RawError::Truncated { .. })
        ));
        // Wrong point index in ASCII values.
        let mut bad = ngspice_ascii();
        let at = bad.windows(3).position(|w| w == b"\n1\t").unwrap();
        bad[at + 1] = b'7';
        assert_eq!(parse_raw(&bad), Err(RawError::Values(1)));
        // Odd-length UTF-16 tail terminates.
        assert!(parse_raw(&[b'T', 0, b'i']).is_err());
        // Multibyte text where a key is compared.
        assert!(parse_raw("Plotnämé: x\nNo. ü".as_bytes()).is_err());
    }
}
