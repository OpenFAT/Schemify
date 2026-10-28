//! Snapshot tables (§4): the app's `doc`/`elab` rows in (filled or spliced per column) →
//! wire-form slices for the `read_column` / `read_strings` imports out. Columns are stored
//! in wire form (little-endian bytes, UTF-8 strings), so every `read_column` is one bounds
//! check and one `memcpy` into guest memory.
//!
//! Contents:
//! - `pub const ROWS_MAX: u32`
//! - `pub struct Snapshots`: `new`, `fill`, `fill_strings`, `splice`, `splice_strings`,
//!   `rows`, `epoch`, `check`; crate-internal `placeholder`, `is_lent`, `has_doc`,
//!   `column_bytes`, `strings_len`, `write_strings`
//! - `pub(crate) fn row_start(ends: &[u32], i: usize) -> usize`

use std::ops::Range;

use schemify_abi::{Code, ColType, Scalar, Table, col};

/// Rows per snapshot table, the largest owner capacity (`inst_paths`, 2^24); keeps every
/// row count and byte length representable in the `i32` imports return.
pub const ROWS_MAX: u32 = 1 << 24;

/// Every §4 table, filled by the caller: per table an epoch and its columns in wire form.
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
}

/// One column in wire form. Fixed width: `bytes.len() == rows * width`, `ends` empty.
/// String: `ends[i]` is the exclusive end of row `i` in `bytes` (non-decreasing).
#[derive(Clone, Debug, Default)]
struct Column {
    bytes: Vec<u8>,
    ends: Vec<u32>,
}

#[derive(Clone, Debug)]
struct TableData {
    /// Advances (wrapping) on every `fill*` / `splice*` of any of its columns.
    epoch: u32,
    columns: Vec<Column>,
}

impl Default for Snapshots {
    fn default() -> Snapshots {
        Snapshots::new()
    }
}

impl Snapshots {
    /// Returns every table empty, at epoch 0.
    #[must_use]
    pub fn new() -> Snapshots {
        let tables: Vec<TableData> = Table::ALL
            .iter()
            .map(|table| TableData {
                epoch: 0,
                columns: vec![Column::default(); table.columns().len()],
            })
            .collect();
        let snapshots = Snapshots { tables };
        debug_assert!(
            snapshots.is_lent(),
            "a full table set, never the placeholder"
        );
        snapshots
    }

    /// Replaces a fixed-width column and advances the table's epoch.
    ///
    /// # Panics
    ///
    /// Panics if `table` has no column `column`, the column is not of type `T`, or
    /// `values.len() > ROWS_MAX`.
    pub fn fill<T: Scalar>(&mut self, table: Table, column: u32, values: &[T]) {
        assert!(values.len() <= ROWS_MAX as usize);
        let target = self.column_mut(table, column, T::TY);
        debug_assert!(target.ends.is_empty(), "a fixed-width column has no ends");
        let width = usize::from(T::TY.width());
        target.bytes.clear();
        // Room for later splices to grow into without reallocating the column.
        target
            .bytes
            .reserve((values.len() + values.len() / 8) * width);
        for &value in values {
            value.put_le(&mut target.bytes);
        }
        debug_assert_eq!(target.bytes.len(), values.len() * width);
    }

    /// Replaces a string column and advances the table's epoch.
    ///
    /// # Panics
    ///
    /// Panics if `table` has no column `column`, the column is not a string column,
    /// `values.len() > ROWS_MAX`, or the strings total 4 GiB or more.
    pub fn fill_strings<S: AsRef<str>>(&mut self, table: Table, column: u32, values: &[S]) {
        assert!(values.len() <= ROWS_MAX as usize);
        let target = self.column_mut(table, column, ColType::Str);
        target.bytes.clear();
        target.ends.clear();
        target.ends.reserve(values.len() + values.len() / 8);
        for value in values {
            target.bytes.extend_from_slice(value.as_ref().as_bytes());
            let end = u32::try_from(target.bytes.len()).expect("string column under 4 GiB");
            target.ends.push(end);
        }
        debug_assert_eq!(target.ends.len(), values.len());
        let last = target.ends.last().map_or(0, |&end| end as usize);
        debug_assert_eq!(last, target.bytes.len(), "the last row ends the column");
    }

    /// Replaces rows `at` of a fixed-width column with `values` (the rows after it shift) and
    /// advances the table's epoch: [`Snapshots::fill`] of the spliced sequence, in O(changed
    /// rows) plus one move of the column's tail. Other columns are not touched: the caller
    /// keeps the table's columns in step ([`Snapshots::check`]).
    ///
    /// # Panics
    ///
    /// Panics if `table` has no column `column`, the column is not of type `T`, `at` is
    /// reversed or past the column's rows, or the result would exceed [`ROWS_MAX`].
    pub fn splice<T: Scalar>(&mut self, table: Table, column: u32, at: Range<usize>, values: &[T]) {
        let target = self.column_mut(table, column, T::TY);
        debug_assert!(target.ends.is_empty(), "a fixed-width column has no ends");
        let width = usize::from(T::TY.width());
        let rows = target.bytes.len() / width;
        assert!(at.start <= at.end, "reversed rows {at:?}");
        assert!(at.end <= rows, "rows {at:?} of {rows}");
        let after = rows - at.len() + values.len();
        assert!(after <= ROWS_MAX as usize);
        let mut encoded = Vec::with_capacity(values.len() * width);
        for &value in values {
            value.put_le(&mut encoded);
        }
        target
            .bytes
            .splice(at.start * width..at.end * width, encoded);
        debug_assert_eq!(target.bytes.len(), after * width);
    }

    /// [`Snapshots::splice`] for a string column.
    ///
    /// # Panics
    ///
    /// Panics if `table` has no column `column`, the column is not a string column, `at` is
    /// reversed or past its rows, or the strings would total 4 GiB or more.
    pub fn splice_strings<S: AsRef<str>>(
        &mut self,
        table: Table,
        column: u32,
        at: Range<usize>,
        values: &[S],
    ) {
        let target = self.column_mut(table, column, ColType::Str);
        let rows = target.ends.len();
        assert!(at.start <= at.end, "reversed rows {at:?}");
        assert!(at.end <= rows, "rows {at:?} of {rows}");
        let first = row_start(&target.ends, at.start);
        let last = row_start(&target.ends, at.end);
        let mut bytes = Vec::new();
        let mut lens = Vec::with_capacity(values.len());
        for value in values {
            bytes.extend_from_slice(value.as_ref().as_bytes());
            lens.push(bytes.len());
        }
        let total = target.bytes.len() - (last - first) + bytes.len();
        assert!(u32::try_from(total).is_ok(), "string column under 4 GiB");
        let grown = bytes.len() as i64 - (last - first) as i64;
        target.bytes.splice(first..last, bytes);
        let new_ends = lens.iter().map(|&len| (first + len) as u32);
        target.ends.splice(at.clone(), new_ends);
        for end in &mut target.ends[at.start + values.len()..] {
            *end = (i64::from(*end) + grown) as u32;
        }
        debug_assert_eq!(target.ends.len(), rows - at.len() + values.len());
        debug_assert!(target.ends.last().is_none_or(|&end| end as usize == total));
        debug_assert!(target.ends.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    /// Returns the row count (that of column 0; [`Snapshots::check`] verifies the rest).
    #[must_use]
    pub fn rows(&self, table: Table) -> u32 {
        rows_of(table, 0, &self.tables[table as usize].columns[0])
    }

    /// Returns the table's epoch; it advances on every `fill*` and `splice*`.
    #[must_use]
    pub fn epoch(&self, table: Table) -> u32 {
        self.tables[table as usize].epoch
    }

    /// Verifies every table's columns hold the same number of rows.
    ///
    /// # Errors
    ///
    /// The first table whose columns disagree.
    ///
    /// # Panics
    ///
    /// Panics on the crate's internal placeholder, which a caller never holds.
    pub fn check(&self) -> Result<(), Table> {
        assert!(self.is_lent(), "placeholder snapshots");
        for &table in Table::ALL {
            let columns = &self.tables[table as usize].columns;
            let rows = rows_of(table, 0, &columns[0]);
            let mut counts = columns
                .iter()
                .enumerate()
                .map(|(i, c)| rows_of(table, i, c));
            if !counts.all(|count| count == rows) {
                return Err(table);
            }
        }
        Ok(())
    }

    /// Allocation-free stand-in kept in a plugin's store while the real value is not lent.
    pub(crate) fn placeholder() -> Snapshots {
        Snapshots { tables: Vec::new() }
    }

    /// Returns whether this is the full table set (not a placeholder).
    pub(crate) fn is_lent(&self) -> bool {
        self.tables.len() == Table::COUNT
    }

    /// Returns whether `doc` is a live handle in the `Docs` table. O(docs).
    pub(crate) fn has_doc(&self, doc: u32) -> bool {
        debug_assert!(self.is_lent(), "placeholder snapshots");
        let docs = &self.tables[Table::Docs as usize];
        let handles = &docs.columns[col::docs::HANDLE as usize];
        debug_assert_eq!(handles.bytes.len() % 4, 0, "`Docs.handle` is a u32 column");
        handles
            .bytes
            .chunks_exact(4)
            .any(|bytes| u32::get_le(bytes) == doc)
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
        let width = usize::from(table.columns()[column as usize].ty.width());
        if width == 0 {
            return Err(Code::Column);
        }
        let rows = row_range(rows_of(table, column as usize, c), start, count)?;
        let bytes = &c.bytes[rows.start * width..rows.end * width];
        debug_assert_eq!(bytes.len(), count as usize * width);
        Ok(bytes)
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
        let (first, last) = (row_start(&c.ends, rows.start), row_start(&c.ends, rows.end));
        debug_assert!(first <= last, "string ends are non-decreasing");
        Ok(4 * rows.len() + (last - first))
    }

    /// Writes the `read_strings` wire form into `dst`.
    ///
    /// # Panics
    ///
    /// Panics unless [`Snapshots::strings_len`] accepted the same arguments and `dst` is
    /// exactly that many bytes.
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
            let string = &c.bytes[row_start(&c.ends, i)..c.ends[i] as usize];
            let len = u32::try_from(string.len()).expect("< 4 GiB");
            dst[at..at + 4].copy_from_slice(&len.to_le_bytes());
            dst[at + 4..at + 4 + string.len()].copy_from_slice(string);
            at += 4 + string.len();
        }
        assert_eq!(at, dst.len(), "`dst` is exactly `strings_len` bytes");
    }
}

/// Start of row `i` in a byte run whose rows end at `ends` (row `i` is `start..ends[i]`).
pub(crate) fn row_start(ends: &[u32], i: usize) -> usize {
    debug_assert!(i <= ends.len());
    i.checked_sub(1).map_or(0, |p| ends[p] as usize)
}

// ---- private ----

impl Snapshots {
    /// The rows `start..start + count` of string column `column`, checked.
    fn string_rows(
        &self,
        table: u32,
        column: u32,
        start: u32,
        count: u32,
    ) -> Result<(Range<usize>, &Column), Code> {
        let (table, c) = self.column(table, column)?;
        if table.columns()[column as usize].ty != ColType::Str {
            return Err(Code::Column);
        }
        let rows = row_range(rows_of(table, column as usize, c), start, count)?;
        debug_assert!(
            rows.end <= c.ends.len(),
            "a string column's rows are its ends"
        );
        Ok((rows, c))
    }

    /// Column `column` of `table` for writing, after checking it holds `ty`; advances the
    /// table's epoch.
    fn column_mut(&mut self, table: Table, column: u32, ty: ColType) -> &mut Column {
        let columns = table.columns();
        let name = table.name();
        assert!(
            (column as usize) < columns.len(),
            "{name} has no column {column}"
        );
        let definition = columns[column as usize];
        let (field, expected) = (definition.name, definition.ty.name());
        assert_eq!(definition.ty, ty, "{name}.{field} is {expected}");
        let data = &mut self.tables[table as usize];
        data.epoch = data.epoch.wrapping_add(1);
        &mut data.columns[column as usize]
    }

    /// Column `column` of raw table id `table`, as a guest names it.
    fn column(&self, table: u32, column: u32) -> Result<(Table, &Column), Code> {
        let table = Table::from_raw(table).ok_or(Code::Table)?;
        debug_assert_eq!(
            self.tables[table as usize].columns.len(),
            table.columns().len(),
            "`new` builds one column per §4 column"
        );
        let c = self.tables[table as usize]
            .columns
            .get(column as usize)
            .ok_or(Code::Column)?;
        Ok((table, c))
    }
}

/// Row count of column `column` of `table`, from its wire form.
fn rows_of(table: Table, column: usize, c: &Column) -> u32 {
    let rows = match table.columns()[column].ty.width() {
        0 => c.ends.len(),
        width => {
            let width = usize::from(width);
            debug_assert_eq!(c.bytes.len() % width, 0, "a column holds whole rows");
            c.bytes.len() / width
        }
    };
    u32::try_from(rows).expect("fill caps rows at ROWS_MAX")
}

/// `start..start + count` when it lies within `rows`.
fn row_range(rows: u32, start: u32, count: u32) -> Result<Range<usize>, Code> {
    let end = start.checked_add(count).ok_or(Code::Range)?;
    if end > rows {
        return Err(Code::Range);
    }
    let range = start as usize..end as usize;
    debug_assert_eq!(range.len(), count as usize);
    Ok(range)
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
        let nine = &9u32.to_le_bytes()[..];
        assert_eq!(s.column_bytes(0, col::docs::HANDLE, 1, 1), Ok(nine));
        assert_eq!(s.column_bytes(0, col::docs::HANDLE, 1, 2), Err(Code::Range));
        let wrap = s.column_bytes(0, col::docs::HANDLE, u32::MAX, 2);
        assert_eq!(wrap, Err(Code::Range));
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
}
