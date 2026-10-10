//! `.raw` file parser — ngspice and LTspice, ascii and binary.
//!
//! Input: raw bytes. Output: `Vec<RawPlot>` (refined type — downstream never
//! re-validates). Handles:
//!   - ngspice ascii (`Values:`) and binary (`Binary:`, row-major f64)
//!   - LTspice binary (UTF-16LE header, scale f64 + data f32, abs-coded time,
//!     `fastaccess` column-major variant, complex AC as f64 pairs)
//!   - multiple analysis blocks appended in one file
//!   - parametric sweep step detection (scale variable restart)

use crate::data::{RawPlot, VarKind, Variable};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RawError {
    #[error("empty file")]
    Empty,
    #[error("malformed header: {0}")]
    Header(String),
    #[error("binary data truncated: expected {expected} bytes, found {found}")]
    Truncated { expected: usize, found: usize },
    #[error("malformed ascii values at point {0}")]
    Values(u32),
    #[error("no plots found in file")]
    NoPlots,
}

/// Parse a complete `.raw` file. Returns one `RawPlot` per analysis block.
pub fn parse_raw(bytes: &[u8]) -> Result<Vec<RawPlot>, RawError> {
    if bytes.is_empty() {
        return Err(RawError::Empty);
    }
    let mut rd = Reader::new(bytes);
    let mut plots = Vec::new();
    while !rd.at_end() {
        match parse_one_plot(&mut rd)? {
            Some(p) => plots.push(p),
            None => break, // trailing whitespace
        }
    }
    if plots.is_empty() {
        return Err(RawError::NoPlots);
    }
    Ok(plots)
}

// ════════════════════════════════════════════════════════════
// Header parsing
// ════════════════════════════════════════════════════════════

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
    let complex = h.flags.to_ascii_lowercase().contains("complex");
    let fastaccess = h.flags.to_ascii_lowercase().contains("fastaccess");
    let ltspice = h.command.to_ascii_lowercase().contains("ltspice")
        || h.command.to_ascii_lowercase().contains("linear technology");
    let n = h.n_points as usize;

    let (re, im) = if h.binary {
        parse_binary(rd, &h, complex, fastaccess, ltspice)?
    } else {
        parse_ascii_values(rd, &h, complex)?
    };

    let mut plot = RawPlot {
        plotname: h.plotname,
        complex,
        variables: h.variables,
        n_points: h.n_points,
        re,
        im,
        steps: Vec::new(),
    };

    // LTspice compresses transient files by flagging non-essential points
    // with a negative time value; consumers take |t|.
    if ltspice && !plot.variables.is_empty() && plot.variables[0].kind == VarKind::Time {
        for x in &mut plot.re[..n] {
            *x = x.abs();
        }
    }

    plot.detect_steps();
    Ok(Some(plot))
}

fn parse_header(rd: &mut Reader) -> Result<Option<Header>, RawError> {
    let mut h = Header::default();
    let mut seen_any = false;
    loop {
        let Some(line) = rd.read_line() else {
            return if seen_any {
                Err(RawError::Header("unexpected EOF in header".into()))
            } else {
                Ok(None)
            };
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        seen_any = true;
        let lower = trimmed.to_ascii_lowercase();
        if let Some(v) = value_of(trimmed, "plotname:") {
            h.plotname = v.to_string();
        } else if let Some(v) = value_of(trimmed, "command:") {
            h.command = v.to_string();
        } else if let Some(v) = value_of(trimmed, "flags:") {
            h.flags = v.to_string();
        } else if let Some(v) = value_of(trimmed, "no. variables:") {
            h.n_vars = v
                .trim()
                .parse()
                .map_err(|_| RawError::Header(format!("bad variable count: {v}")))?;
        } else if let Some(v) = value_of(trimmed, "no. points:") {
            h.n_points = v
                .trim()
                .parse()
                .map_err(|_| RawError::Header(format!("bad point count: {v}")))?;
        } else if lower.starts_with("variables:") {
            // Followed by n_vars lines: "<idx> <name> <type> [params...]".
            if h.n_vars == 0 {
                return Err(RawError::Header("Variables: before No. Variables:".into()));
            }
            h.variables.reserve(h.n_vars as usize);
            for _ in 0..h.n_vars {
                let vl = rd
                    .read_line()
                    .ok_or_else(|| RawError::Header("EOF in variable list".into()))?;
                let mut it = vl.split_whitespace();
                let _idx = it.next();
                let name = it
                    .next()
                    .ok_or_else(|| RawError::Header(format!("bad variable line: {vl}")))?;
                let kind = it
                    .next()
                    .map(VarKind::from_type_str)
                    .unwrap_or(VarKind::Other);
                h.variables.push(Variable {
                    name: name.to_string(),
                    kind,
                });
            }
        } else if lower.starts_with("values:") {
            h.binary = false;
            break;
        } else if lower.starts_with("binary:") {
            h.binary = true;
            break;
        }
        // Unknown header lines (Date:, Offset:, Dimensions:, …) are skipped.
    }
    if h.variables.len() != h.n_vars as usize {
        return Err(RawError::Header(format!(
            "variable count mismatch: declared {}, listed {}",
            h.n_vars,
            h.variables.len()
        )));
    }
    if h.n_points == 0 || h.n_vars == 0 {
        return Err(RawError::Header("zero points or variables".into()));
    }
    Ok(Some(h))
}

/// Case-insensitive `"Key:" value` extractor.
fn value_of<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    if line.len() >= key.len() && line[..key.len()].eq_ignore_ascii_case(key) {
        Some(line[key.len()..].trim())
    } else {
        None
    }
}

// ════════════════════════════════════════════════════════════
// Ascii values
// ════════════════════════════════════════════════════════════

/// Parse the `Values:` section: per point, `<idx> <v0>` then one value per
/// line. Complex values are `re,im`. Output is column-major.
fn parse_ascii_values(
    rd: &mut Reader,
    h: &Header,
    complex: bool,
) -> Result<(Vec<f64>, Vec<f64>), RawError> {
    let n = h.n_points as usize;
    let nv = h.n_vars as usize;
    let mut re = vec![0.0f64; nv * n];
    let mut im = if complex {
        vec![0.0f64; nv * n]
    } else {
        Vec::new()
    };

    let mut point = 0usize;
    let mut var = 0usize;
    while point < n {
        let Some(line) = rd.read_line() else {
            return Err(RawError::Values(point as u32));
        };
        let mut rest = line.trim();
        if rest.is_empty() {
            continue;
        }
        // First value of a point is prefixed with the point index.
        if var == 0 {
            let mut it = rest.splitn(2, char::is_whitespace);
            let _idx = it.next();
            rest = it.next().unwrap_or("").trim();
            if rest.is_empty() {
                return Err(RawError::Values(point as u32));
            }
        }
        // A line may hold one value (ngspice) — parse all tokens present.
        for tok in rest.split_whitespace() {
            if var >= nv {
                return Err(RawError::Values(point as u32));
            }
            let (r, i) = parse_value(tok).ok_or(RawError::Values(point as u32))?;
            re[var * n + point] = r;
            if complex {
                im[var * n + point] = i;
            }
            var += 1;
        }
        if var == nv {
            var = 0;
            point += 1;
        }
    }
    Ok((re, im))
}

/// `"1.5"` → (1.5, 0) · `"1.5,-2.5"` → (1.5, -2.5)
fn parse_value(tok: &str) -> Option<(f64, f64)> {
    match tok.split_once(',') {
        Some((r, i)) => Some((r.trim().parse().ok()?, i.trim().parse().ok()?)),
        None => Some((tok.trim().parse().ok()?, 0.0)),
    }
}

// ════════════════════════════════════════════════════════════
// Binary values
// ════════════════════════════════════════════════════════════

/// Binary layouts, all little-endian:
///   - ngspice real:      row-major, every value f64
///   - ngspice complex:   row-major, every value (f64, f64)
///   - LTspice real:      row-major, scale var f64, data vars f32
///   - LTspice fastaccess: column-major, scale f64 column, data f32 columns
///   - LTspice complex:   row-major, every value (f64, f64)
///   - LTspice "double":  row-major, every value f64 (detected by size)
fn parse_binary(
    rd: &mut Reader,
    h: &Header,
    complex: bool,
    fastaccess: bool,
    ltspice: bool,
) -> Result<(Vec<f64>, Vec<f64>), RawError> {
    let n = h.n_points as usize;
    let nv = h.n_vars as usize;

    if complex {
        let expected = n * nv * 16;
        let data = rd.take_bytes(expected)?;
        let mut re = vec![0.0f64; nv * n];
        let mut im = vec![0.0f64; nv * n];
        // Row-major pairs → column-major split.
        for p in 0..n {
            for v in 0..nv {
                let off = (p * nv + v) * 16;
                re[v * n + p] = f64_le(&data[off..]);
                im[v * n + p] = f64_le(&data[off + 8..]);
            }
        }
        return Ok((re, im));
    }

    let all_f64 = n * nv * 8;
    let mixed = n * (8 + (nv - 1) * 4); // LTspice: scale f64 + data f32
    let avail = rd.bytes.len() - rd.pos; // size heuristic: f32 vs f64 data

    let mut re = vec![0.0f64; nv * n];
    if ltspice && avail >= mixed && (avail < all_f64 || nv == 1) {
        let data = rd.take_bytes(mixed)?;
        if fastaccess {
            // Column-major: scale column f64, then each data column f32.
            for p in 0..n {
                re[p] = f64_le(&data[p * 8..]);
            }
            let mut off = n * 8;
            for v in 1..nv {
                for p in 0..n {
                    re[v * n + p] = f32_le(&data[off..]) as f64;
                    off += 4;
                }
            }
        } else {
            // Row-major: per point, f64 scale + f32 data.
            let stride = 8 + (nv - 1) * 4;
            for p in 0..n {
                let row = p * stride;
                re[p] = f64_le(&data[row..]);
                for v in 1..nv {
                    re[v * n + p] = f32_le(&data[row + 8 + (v - 1) * 4..]) as f64;
                }
            }
        }
    } else {
        // ngspice (or LTspice double-precision): row-major f64.
        let data = rd.take_bytes(all_f64)?;
        for p in 0..n {
            for v in 0..nv {
                re[v * n + p] = f64_le(&data[(p * nv + v) * 8..]);
            }
        }
    }
    Ok((re, vec![]))
}

#[inline]
fn f64_le(b: &[u8]) -> f64 {
    f64::from_le_bytes(b[..8].try_into().unwrap())
}

#[inline]
fn f32_le(b: &[u8]) -> f32 {
    f32::from_le_bytes(b[..4].try_into().unwrap())
}

// ════════════════════════════════════════════════════════════
// Reader — line-oriented over UTF-8 or UTF-16LE bytes, with raw byte access
// for binary sections.
// ════════════════════════════════════════════════════════════

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    utf16: bool,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        // UTF-16LE detection: BOM, or ascii first char with zero high byte.
        let utf16 = bytes.len() >= 2 && (bytes[..2] == [0xFF, 0xFE] || bytes[1] == 0);
        let pos = if bytes.len() >= 2 && bytes[..2] == [0xFF, 0xFE] {
            2
        } else {
            0
        };
        Self { bytes, pos, utf16 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    /// Read one text line (up to `\n`), stripping `\r`. Returns `None` at EOF.
    fn read_line(&mut self) -> Option<String> {
        if self.at_end() {
            return None;
        }
        let mut out = String::new();
        if self.utf16 {
            while self.pos + 1 < self.bytes.len() {
                let u = u16::from_le_bytes([self.bytes[self.pos], self.bytes[self.pos + 1]]);
                self.pos += 2;
                if u == b'\n' as u16 {
                    break;
                }
                if u != b'\r' as u16 {
                    // .raw headers are ascii in practice; lossy is fine.
                    out.push(char::from_u32(u as u32).unwrap_or('\u{FFFD}'));
                }
            }
        } else {
            while self.pos < self.bytes.len() {
                let b = self.bytes[self.pos];
                self.pos += 1;
                if b == b'\n' {
                    break;
                }
                if b != b'\r' {
                    out.push(b as char);
                }
            }
        }
        Some(out)
    }

    /// Consume exactly `want` bytes; error if fewer available.
    fn take_bytes(&mut self, want: usize) -> Result<&'a [u8], RawError> {
        let avail = self.bytes.len() - self.pos;
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

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

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
        assert_eq!(p.n_points, 4);
        assert_eq!(p.variables.len(), 3);
        assert_eq!(p.variables[1].name, "v(out)");
        assert_eq!(p.variables[1].kind, VarKind::Voltage);
        assert_eq!(p.scale(), &[0.0, 1e-9, 2e-9, 3e-9]);
        assert_eq!(p.col(1), &[0.0, 0.5, 0.8, 0.95]);
        assert_eq!(p.col(2), &[1e-3, 2e-3, 3e-3, 4e-3]);
        assert_eq!(p.steps, vec![0..4]);
        assert_eq!(p.find_var("V(OUT)"), Some(1)); // case-insensitive
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
        let plots = parse_raw(&ngspice_binary(&rows)).unwrap();
        let p = &plots[0];
        assert_eq!(p.scale(), &[0.0, 1e-9, 2e-9]);
        assert_eq!(p.col(1), &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn binary_ltspice_utf16_f32_abs_time() {
        // UTF-16LE header, row-major: time f64 (negative = compression
        // marker), data f32.
        let header = "Title: * tb\nDate: x\nPlotname: Transient Analysis\nFlags: real forward\nNo. Variables: 2\nNo. Points: 3\nOffset: 0\nCommand: Linear Technology LTspice XVII\nVariables:\n\t0\ttime\ttime\n\t1\tV(out)\tvoltage\nBinary:\n";
        let mut f: Vec<u8> = Vec::new();
        for c in header.encode_utf16() {
            f.extend_from_slice(&c.to_le_bytes());
        }
        let pts: [(f64, f32); 3] = [(0.0, 0.1), (-1e-9, 0.2), (2e-9, 0.3)];
        for (t, v) in pts {
            f.extend_from_slice(&t.to_le_bytes());
            f.extend_from_slice(&v.to_le_bytes());
        }
        let plots = parse_raw(&f).unwrap();
        let p = &plots[0];
        assert_eq!(p.scale(), &[0.0, 1e-9, 2e-9]); // abs applied
        let col = p.col(1);
        assert!((col[0] - 0.1).abs() < 1e-6);
        assert!((col[2] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn complex_ascii_ac() {
        let f = b"Title: ac
Plotname: AC Analysis
Flags: complex
No. Variables: 2
No. Points: 2
Variables:
\t0\tfrequency\tfrequency
\t1\tv(out)\tvoltage
Values:
0\t1.0,0.0
\t0.5,-0.5
1\t10.0,0.0
\t0.1,-0.2
";
        let plots = parse_raw(f).unwrap();
        let p = &plots[0];
        assert!(p.complex);
        assert_eq!(p.scale(), &[1.0, 10.0]);
        assert_eq!(p.col(1), &[0.5, 0.1]);
        assert_eq!(p.col_im(1), &[-0.5, -0.2]);
    }

    #[test]
    fn sweep_steps_detected() {
        // Two steps of 3 points: time restarts at 0.
        let rows = [
            [0.0, 1.0],
            [1e-9, 2.0],
            [2e-9, 3.0],
            [0.0, 10.0],
            [1e-9, 20.0],
            [2e-9, 30.0],
        ];
        let plots = parse_raw(&ngspice_binary(&rows)).unwrap();
        assert_eq!(plots[0].steps, vec![0..3, 3..6]);
    }

    #[test]
    fn multiple_blocks_appended() {
        let mut f = ngspice_ascii();
        f.extend_from_slice(&ngspice_ascii());
        let plots = parse_raw(&f).unwrap();
        assert_eq!(plots.len(), 2);
        assert_eq!(plots[1].col(1), &[0.0, 0.5, 0.8, 0.95]);
    }

    #[test]
    fn truncated_binary_errors() {
        let mut f = ngspice_binary(&[[0.0, 1.0], [1e-9, 2.0]]);
        f.truncate(f.len() - 4);
        assert!(matches!(parse_raw(&f), Err(RawError::Truncated { .. })));
    }
}
