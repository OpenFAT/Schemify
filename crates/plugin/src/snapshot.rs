//! Snapshot tables (§4): plain data the app fills from `doc`/`elab` tables and lends to
//! [`crate::deliver`]. Columns are stored in wire form (little-endian bytes, UTF-8 strings)
//! so every `read_column` is one bounds check and one `memcpy` into guest memory.

use schemify_abi::{Code, ColType, Scalar, Table, col};

/// Rows per snapshot table, the largest owner capacity (`inst_paths`, 2^24); keeps every
/// row count and byte length representable in the `i32` imports return.
pub const ROWS_MAX: u32 = 1 << 24;

/// One column in wire form. Fixed width: `bytes.len() == rows * width`, `ends` empty.
/// String: `ends[i]` is the exclusive end of row `i` in `bytes`.
#[derive(Clone, Debug, Default)]
struct Column {
    bytes: Vec<u8>,
    ends: Vec<u32>,
}

#[derive(Clone, Debug)]
struct TableData {
    epoch: u32,
    columns: Vec<Column>,
}

/// Document records for `read_record`, sorted by `(doc, id)`.
#[derive(Clone, Debug, Default)]
struct Records {
    key: Vec<u64>,
    ends: Vec<u32>,
    text: Vec<u8>,
}

/// Every §4 table plus the `read_record` source, filled by the caller.
///
/// Mutate between deliveries only; [`crate::deliver`] lends the value to each plugin call
/// and hands it back unchanged.
///
/// # Examples
///
/// ```
/// use schemify_plugin::Snapshots;
/// use schemify_plugin::abi::{Table, col};
///
/// let mut snap = Snapshots::new();
/// snap.fill_strings(Table::Nets, col::nets::NAME, &["vdd", "out"]);
/// snap.fill::<u16>(Table::Nets, col::nets::WIDTH, &[1, 1]);
/// snap.fill::<u8>(Table::Nets, col::nets::GLOBAL, &[1, 0]);
/// snap.fill::<u32>(Table::Nets, col::nets::PIN_COUNT, &[4, 1]);
/// assert_eq!(snap.rows(Table::Nets), 2);
/// assert_eq!(snap.check(), Ok(()));
/// ```
#[derive(Clone, Debug)]
pub struct Snapshots {
    tables: Vec<TableData>,
    records: Records,
}

impl Default for Snapshots {
    fn default() -> Snapshots {
        Snapshots::new()
    }
}

impl Snapshots {
    /// Returns every table empty, at epoch 0.
    pub fn new() -> Snapshots {
        let tables = Table::ALL
            .iter()
            .map(|t| TableData {
                epoch: 0,
                columns: vec![Column::default(); t.columns().len()],
            })
            .collect();
        Snapshots {
            tables,
            records: Records::default(),
        }
    }

    /// Allocation-free stand-in kept in a plugin's store while the real value is not lent.
    pub(crate) fn placeholder() -> Snapshots {
        Snapshots {
            tables: Vec::new(),
            records: Records::default(),
        }
    }

    /// Returns whether this is the full table set (not a placeholder).
    pub(crate) fn is_lent(&self) -> bool {
        self.tables.len() == Table::COUNT
    }

    /// Replaces a fixed-width column and advances the table's epoch.
    ///
    /// # Panics
    ///
    /// Panics if the column is not of type `T` or `values.len() > ROWS_MAX`.
    pub fn fill<T: Scalar>(&mut self, table: Table, column: u32, values: &[T]) {
        let def = table.columns()[column as usize];
        assert_eq!(
            def.ty,
            T::TY,
            "{}.{} is {}",
            table.name(),
            def.name,
            def.ty.name()
        );
        assert!(values.len() <= ROWS_MAX as usize);
        let t = &mut self.tables[table as usize];
        let c = &mut t.columns[column as usize];
        c.bytes.clear();
        c.bytes.reserve(values.len() * usize::from(T::TY.width()));
        for &v in values {
            v.put_le(&mut c.bytes);
        }
        t.epoch = t.epoch.wrapping_add(1);
        debug_assert_eq!(c.bytes.len(), values.len() * usize::from(T::TY.width()));
    }

    /// Replaces a string column and advances the table's epoch.
    ///
    /// # Panics
    ///
    /// Panics if the column is not a string column, `values.len() > ROWS_MAX`, or the
    /// strings total 4 GiB or more.
    pub fn fill_strings<S: AsRef<str>>(&mut self, table: Table, column: u32, values: &[S]) {
        let def = table.columns()[column as usize];
        assert_eq!(
            def.ty,
            ColType::Str,
            "{}.{} is {}",
            table.name(),
            def.name,
            def.ty.name()
        );
        assert!(values.len() <= ROWS_MAX as usize);
        let t = &mut self.tables[table as usize];
        let c = &mut t.columns[column as usize];
        c.bytes.clear();
        c.ends.clear();
        for v in values {
            c.bytes.extend_from_slice(v.as_ref().as_bytes());
            c.ends
                .push(u32::try_from(c.bytes.len()).expect("string column under 4 GiB"));
        }
        t.epoch = t.epoch.wrapping_add(1);
        debug_assert_eq!(c.ends.len(), values.len());
    }

    /// Replaces rows `at` of a fixed-width column with `values` (the rows after it shift) and
    /// advances the table's epoch: [`Snapshots::fill`] of the spliced sequence, in O(changed
    /// rows) plus one move of the column's tail. Other columns are not touched: the caller
    /// keeps the table's columns in step ([`Snapshots::check`]).
    ///
    /// # Panics
    ///
    /// Panics if the column is not of type `T`, `at` is past the column's rows, or the result
    /// would exceed [`ROWS_MAX`].
    pub fn splice<T: Scalar>(
        &mut self,
        table: Table,
        column: u32,
        at: std::ops::Range<usize>,
        values: &[T],
    ) {
        let def = table.columns()[column as usize];
        assert_eq!(
            def.ty,
            T::TY,
            "{}.{} is {}",
            table.name(),
            def.name,
            def.ty.name()
        );
        let t = &mut self.tables[table as usize];
        let c = &mut t.columns[column as usize];
        let w = usize::from(T::TY.width());
        let rows = c.bytes.len() / w;
        assert!(
            at.start <= at.end && at.end <= rows,
            "rows {at:?} of {rows}"
        );
        assert!(rows - at.len() + values.len() <= ROWS_MAX as usize);
        let mut enc = Vec::with_capacity(values.len() * w);
        for &v in values {
            v.put_le(&mut enc);
        }
        c.bytes.splice(at.start * w..at.end * w, enc);
        t.epoch = t.epoch.wrapping_add(1);
        debug_assert_eq!(c.bytes.len(), (rows - at.len() + values.len()) * w);
    }

    /// [`Snapshots::splice`] for a string column.
    ///
    /// # Panics
    ///
    /// Panics if the column is not a string column, `at` is past its rows, or the strings
    /// would total 4 GiB or more.
    pub fn splice_strings<S: AsRef<str>>(
        &mut self,
        table: Table,
        column: u32,
        at: std::ops::Range<usize>,
        values: &[S],
    ) {
        let def = table.columns()[column as usize];
        assert_eq!(
            def.ty,
            ColType::Str,
            "{}.{} is {}",
            table.name(),
            def.name,
            def.ty.name()
        );
        let t = &mut self.tables[table as usize];
        let c = &mut t.columns[column as usize];
        let rows = c.ends.len();
        assert!(
            at.start <= at.end && at.end <= rows,
            "rows {at:?} of {rows}"
        );
        let end_of = |i: usize| if i == 0 { 0 } else { c.ends[i - 1] as usize };
        let (b0, b1) = (end_of(at.start), end_of(at.end));
        let mut bytes = Vec::new();
        let mut ends = Vec::with_capacity(values.len());
        for v in values {
            bytes.extend_from_slice(v.as_ref().as_bytes());
            ends.push(b0 + bytes.len());
        }
        let grown = bytes.len() as i64 - (b1 - b0) as i64;
        c.bytes.splice(b0..b1, bytes);
        let total = u32::try_from(c.bytes.len()).expect("string column under 4 GiB");
        let ends: Vec<u32> = ends.into_iter().map(|e| e as u32).collect();
        let n = ends.len();
        c.ends.splice(at.clone(), ends);
        for e in &mut c.ends[at.start + n..] {
            *e = (i64::from(*e) + grown) as u32;
        }
        t.epoch = t.epoch.wrapping_add(1);
        debug_assert!(c.ends.last().is_none_or(|&e| e == total));
        debug_assert!(c.ends.windows(2).all(|p| p[0] <= p[1]));
    }

    /// Returns the row count (that of column 0; [`Snapshots::check`] verifies the rest).
    pub fn rows(&self, table: Table) -> u32 {
        let t = &self.tables[table as usize];
        rows_of(table, 0, &t.columns[0])
    }

    /// Returns the table's epoch; it advances on every `fill*`.
    pub fn epoch(&self, table: Table) -> u32 {
        self.tables[table as usize].epoch
    }

    /// Verifies every table's columns hold the same number of rows.
    ///
    /// # Errors
    ///
    /// The first table whose columns disagree.
    pub fn check(&self) -> Result<(), Table> {
        assert!(self.is_lent(), "placeholder snapshots");
        for &table in Table::ALL {
            let t = &self.tables[table as usize];
            let rows = rows_of(table, 0, &t.columns[0]);
            let ok = t
                .columns
                .iter()
                .enumerate()
                .all(|(i, c)| rows_of(table, i, c) == rows);
            if !ok {
                return Err(table);
            }
        }
        Ok(())
    }

    /// Adds one document record (line-record text) for `read_record`.
    ///
    /// # Panics
    ///
    /// Panics unless `(doc, id)` is greater than every key pushed since the last
    /// [`Snapshots::clear_records`] (callers push in sorted order; lookups binary-search).
    pub fn push_record(&mut self, doc: u32, id: u32, text: &str) {
        let key = (u64::from(doc) << 32) | u64::from(id);
        let r = &mut self.records;
        assert!(
            r.key.last().is_none_or(|&last| last < key),
            "records must be pushed sorted"
        );
        r.text.extend_from_slice(text.as_bytes());
        r.key.push(key);
        r.ends
            .push(u32::try_from(r.text.len()).expect("records under 4 GiB"));
        debug_assert_eq!(r.key.len(), r.ends.len());
    }

    /// Drops every record.
    pub fn clear_records(&mut self) {
        self.records = Records::default();
    }

    /// Returns the text of record `(doc, id)`, or `None`.
    pub(crate) fn record(&self, doc: u32, id: u32) -> Option<&[u8]> {
        let r = &self.records;
        let i = r
            .key
            .binary_search(&((u64::from(doc) << 32) | u64::from(id)))
            .ok()?;
        let start = if i == 0 { 0 } else { r.ends[i - 1] as usize };
        Some(&r.text[start..r.ends[i] as usize])
    }

    /// Returns whether `doc` is a live handle in the `Docs` table.
    pub(crate) fn has_doc(&self, doc: u32) -> bool {
        let handles = &self.tables[Table::Docs as usize].columns[col::docs::HANDLE as usize];
        handles.bytes.chunks_exact(4).any(|b| u32::get_le(b) == doc)
    }

    /// Resolves a fixed-width column slice in wire form.
    ///
    /// # Errors
    ///
    /// `E_TABLE`, `E_COLUMN` (unknown or string column), `E_RANGE` (rows past the end).
    pub(crate) fn column_bytes(
        &self,
        table: u32,
        column: u32,
        start: u32,
        count: u32,
    ) -> Result<&[u8], Code> {
        let (table, c) = self.column(table, column)?;
        let width = table.columns()[column as usize].ty.width();
        if width == 0 {
            return Err(Code::Column);
        }
        let rows = row_range(rows_of(table, column as usize, c), start, count)?;
        let w = usize::from(width);
        Ok(&c.bytes[rows.start * w..rows.end * w])
    }

    /// Returns the size in bytes of the `read_strings` wire form (`u32 len` + bytes per row).
    ///
    /// # Errors
    ///
    /// As [`Snapshots::column_bytes`], with `E_COLUMN` for a fixed-width column.
    pub(crate) fn strings_len(
        &self,
        table: u32,
        column: u32,
        start: u32,
        count: u32,
    ) -> Result<usize, Code> {
        let (rows, c) = self.string_rows(table, column, start, count)?;
        let first = if rows.start == 0 {
            0
        } else {
            c.ends[rows.start - 1] as usize
        };
        let last = if rows.end == 0 {
            0
        } else {
            c.ends[rows.end - 1] as usize
        };
        debug_assert!(first <= last, "string ends are non-decreasing");
        Ok(4 * rows.len() + (last - first))
    }

    /// Writes the `read_strings` wire form into `dst`, which must be exactly
    /// [`Snapshots::strings_len`] bytes.
    pub(crate) fn write_strings(
        &self,
        table: u32,
        column: u32,
        start: u32,
        count: u32,
        dst: &mut [u8],
    ) {
        let (rows, c) = self
            .string_rows(table, column, start, count)
            .expect("validated by strings_len");
        let mut at = 0;
        for i in rows {
            let begin = if i == 0 { 0 } else { c.ends[i - 1] as usize };
            let s = &c.bytes[begin..c.ends[i] as usize];
            dst[at..at + 4]
                .copy_from_slice(&u32::try_from(s.len()).expect("< 4 GiB").to_le_bytes());
            dst[at + 4..at + 4 + s.len()].copy_from_slice(s);
            at += 4 + s.len();
        }
        assert_eq!(at, dst.len());
    }

    fn string_rows(
        &self,
        table: u32,
        column: u32,
        start: u32,
        count: u32,
    ) -> Result<(std::ops::Range<usize>, &Column), Code> {
        let (table, c) = self.column(table, column)?;
        if table.columns()[column as usize].ty != ColType::Str {
            return Err(Code::Column);
        }
        Ok((
            row_range(rows_of(table, column as usize, c), start, count)?,
            c,
        ))
    }

    fn column(&self, table: u32, column: u32) -> Result<(Table, &Column), Code> {
        let table = Table::from_raw(table).ok_or(Code::Table)?;
        let c = self.tables[table as usize]
            .columns
            .get(column as usize)
            .ok_or(Code::Column)?;
        Ok((table, c))
    }
}

fn rows_of(table: Table, column: usize, c: &Column) -> u32 {
    let n = match table.columns()[column].ty.width() {
        0 => c.ends.len(),
        w => c.bytes.len() / usize::from(w),
    };
    u32::try_from(n).expect("fill caps rows at ROWS_MAX")
}

fn row_range(rows: u32, start: u32, count: u32) -> Result<std::ops::Range<usize>, Code> {
    let end = start.checked_add(count).ok_or(Code::Range)?;
    if end > rows {
        return Err(Code::Range);
    }
    Ok(start as usize..end as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_slice_and_validate() {
        let mut s = Snapshots::new();
        s.fill::<u32>(Table::Docs, col::docs::HANDLE, &[7, 9]);
        s.fill::<u8>(Table::Docs, col::docs::KIND, &[1, 2]);
        s.fill::<u8>(Table::Docs, col::docs::DIRTY, &[0, 1]);
        s.fill_strings(Table::Docs, col::docs::PATH, &["a.shcomp", "b.shbench"]);
        assert_eq!(s.check(), Err(Table::Docs));
        s.fill_strings(Table::Docs, col::docs::CELL, &["a", ""]);
        assert_eq!(s.check(), Ok(()));
        assert_eq!(s.rows(Table::Docs), 2);
        assert_eq!(s.epoch(Table::Docs), 5);
        assert!(s.has_doc(9) && !s.has_doc(8));
        assert_eq!(
            s.column_bytes(0, col::docs::HANDLE, 1, 1),
            Ok(&9u32.to_le_bytes()[..])
        );
        assert_eq!(s.column_bytes(0, col::docs::HANDLE, 1, 2), Err(Code::Range));
        assert_eq!(
            s.column_bytes(0, col::docs::HANDLE, u32::MAX, 2),
            Err(Code::Range)
        );
        assert_eq!(s.column_bytes(0, col::docs::PATH, 0, 1), Err(Code::Column));
        assert_eq!(s.column_bytes(0, 99, 0, 0), Err(Code::Column));
        assert_eq!(s.column_bytes(99, 0, 0, 0), Err(Code::Table));
        let n = s.strings_len(0, col::docs::CELL, 0, 2).unwrap();
        assert_eq!(n, 4 + 1 + 4);
        let mut buf = vec![0; n];
        s.write_strings(0, col::docs::CELL, 0, 2, &mut buf);
        assert_eq!(buf, [1, 0, 0, 0, b'a', 0, 0, 0, 0]);
        assert_eq!(s.strings_len(0, col::docs::CELL, 2, 0), Ok(0));
        assert_eq!(s.strings_len(0, col::docs::HANDLE, 0, 1), Err(Code::Column));
    }

    /// Oracle: random splices of a fixed-width and a string column equal fills of the same
    /// spliced sequences, and each advances the epoch.
    #[test]
    fn splices_equal_fills_of_the_spliced_rows() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        let (mut ids, mut names): (Vec<u32>, Vec<String>) = (Vec::new(), Vec::new());
        let mut s = Snapshots::new();
        for step in 0..400 {
            let n = ids.len() as u64;
            let a = next(n + 1) as usize;
            let b = a + next(n - a as u64 + 1).min(3) as usize;
            let k = next(4) as usize;
            let new_ids: Vec<u32> = (0..k).map(|_| next(1000) as u32).collect();
            let new_names: Vec<String> = (0..k).map(|_| "x".repeat(next(5) as usize)).collect();
            ids.splice(a..b, new_ids.iter().copied());
            names.splice(a..b, new_names.iter().cloned());
            let epoch = s.epoch(Table::Wires);
            s.splice::<u32>(Table::Wires, col::wires::ID, a..b, &new_ids);
            s.splice_strings(Table::Labels, col::labels::TEXT, a..b, &new_names);
            assert_eq!(s.epoch(Table::Wires), epoch.wrapping_add(1), "step {step}");
            let mut f = Snapshots::new();
            f.fill::<u32>(Table::Wires, col::wires::ID, &ids);
            f.fill_strings(Table::Labels, col::labels::TEXT, &names);
            let col =
                |x: &Snapshots, t: Table, c: u32| x.tables[t as usize].columns[c as usize].clone();
            for (t, c) in [
                (Table::Wires, col::wires::ID),
                (Table::Labels, col::labels::TEXT),
            ] {
                let (got, want) = (col(&s, t, c), col(&f, t, c));
                assert_eq!(
                    (got.bytes, got.ends),
                    (want.bytes, want.ends),
                    "step {step}"
                );
            }
        }
    }

    #[test]
    fn records_binary_search() {
        let mut s = Snapshots::new();
        s.push_record(1, 5, "instance R1");
        s.push_record(2, 1, "wire w");
        assert_eq!(s.record(2, 1), Some(&b"wire w"[..]));
        assert_eq!(s.record(1, 5), Some(&b"instance R1"[..]));
        assert_eq!(s.record(1, 6), None);
        s.clear_records();
        assert_eq!(s.record(2, 1), None);
    }

    #[test]
    #[should_panic(expected = "sorted")]
    fn records_must_be_sorted() {
        let mut s = Snapshots::new();
        s.push_record(2, 1, "a");
        s.push_record(1, 1, "b");
    }
}
