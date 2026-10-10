//! Snapshot tables (§4): ids, column ids and column types, fixed per ABI version.
//!
//! Tables are exposed as their owners store them: fixed-width columns as little-endian
//! arrays, string columns as UTF-8. New columns may only be appended in a later version.
//! [`Table::columns`] is the schema; [`col`] names every column id.

use crate::named_enum;

named_enum! {
    /// Column element types. Fixed-width columns are read with `read_column`, strings with
    /// `read_strings`.
    pub enum ColType {
        /// `u8`, 1 byte.
        U8 = "u8",
        /// `u16`, 2 bytes little-endian.
        U16 = "u16",
        /// `u32`, 4 bytes little-endian.
        U32 = "u32",
        /// `i32`, 4 bytes little-endian.
        I32 = "i32",
        /// UTF-8 string, variable width.
        Str = "str",
    }
}

impl ColType {
    /// Returns the element width in bytes; 0 for [`ColType::Str`] (variable).
    pub const fn width(self) -> u8 {
        match self {
            ColType::U8 => 1,
            ColType::U16 => 2,
            ColType::U32 | ColType::I32 => 4,
            ColType::Str => 0,
        }
    }
}

/// One column of a table schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnDef {
    /// Spec name (`pin_count`).
    pub name: &'static str,
    /// Element type.
    pub ty: ColType,
}

mod sealed {
    pub trait Sealed {}
}

/// Fixed-width column element: `u8`, `u16`, `u32` or `i32`.
///
/// Sealed: the set of column types is part of the ABI.
pub trait Scalar: Copy + Default + sealed::Sealed + 'static {
    /// The column type this element reads and writes.
    const TY: ColType;
    /// Appends `self` little-endian.
    fn put_le(self, out: &mut Vec<u8>);
    /// Reads one element from exactly `TY.width()` little-endian bytes.
    ///
    /// # Panics
    ///
    /// Panics if `bytes.len() != Self::TY.width()`.
    fn get_le(bytes: &[u8]) -> Self;
}

macro_rules! scalar {
    ($($t:ty => $ty:ident),*) => {$(
        impl sealed::Sealed for $t {}
        impl Scalar for $t {
            const TY: ColType = ColType::$ty;
            fn put_le(self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            fn get_le(bytes: &[u8]) -> Self {
                <$t>::from_le_bytes(bytes.try_into().expect("element width"))
            }
        }
    )*};
}
scalar!(u8 => U8, u16 => U16, u32 => U32, i32 => I32);

/// Declares [`Table`], its schema and the [`col`] id constants from one list, so ids,
/// names and types cannot drift apart.
macro_rules! tables {
    ($(
        $(#[$tm:meta])* $T:ident = $tname:literal, $m:ident {
            $($C:ident $cname:literal $ty:ident,)*
        }
    )*) => {
        /// Snapshot table ids (§4). The `u8` value is the `table` argument of every import.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u8)]
        pub enum Table { $($(#[$tm])* $T,)* }

        impl Table {
            /// Every table in id order (`ALL[i] as usize == i`).
            pub const ALL: &'static [Table] = &[$(Table::$T,)*];
            /// Number of tables.
            pub const COUNT: usize = Table::ALL.len();

            /// Returns the spec name.
            pub const fn name(self) -> &'static str {
                match self { $(Table::$T => $tname,)* }
            }

            /// Returns the schema: one entry per column id, in id order.
            pub const fn columns(self) -> &'static [ColumnDef] {
                match self {
                    $(Table::$T => &[$(ColumnDef { name: $cname, ty: ColType::$ty },)*],)*
                }
            }

            /// Returns the table with id `raw`; `None` (→ `E_TABLE`) when out of range.
            pub fn from_raw(raw: u32) -> Option<Table> {
                Table::ALL.get(usize::try_from(raw).ok()?).copied()
            }
        }

        /// Column ids per table, `col::<table>::<COLUMN>`; values index [`Table::columns`].
        pub mod col {
            $(
                #[doc = concat!("Columns of [`Table::", stringify!($T), "`](super::Table::", stringify!($T), ").")]
                pub mod $m {
                    #[allow(clippy::upper_case_acronyms, non_camel_case_types)]
                    #[repr(u32)]
                    enum Id { $($C,)* }
                    $(
                        #[doc = concat!("`", $cname, "` (", stringify!($ty), ").")]
                        pub const $C: u32 = Id::$C as u32;
                    )*
                }
            )*
        }
    };
}

tables! {
    /// Open documents.
    Docs = "docs", docs {
        HANDLE "handle" U32, KIND "kind" U8, DIRTY "dirty" U8, PATH "path" Str, CELL "cell" Str,
    }
    /// Instances of the active document.
    Instances = "instances", instances {
        DOC "doc" U32, ID "id" U32, X "x" I32, Y "y" I32, ORIENT "orient" U8, CELL "cell" Str,
        NAME "name" Str,
    }
    /// Wires of the active document.
    Wires = "wires", wires {
        DOC "doc" U32, ID "id" U32, X1 "x1" I32, Y1 "y1" I32, X2 "x2" I32, Y2 "y2" I32,
        NET "net" U32,
    }
    /// Labels of the active document.
    Labels = "labels", labels {
        DOC "doc" U32, ID "id" U32, X "x" I32, Y "y" I32, NET "net" U32, TEXT "text" Str,
    }
    /// Nets of the active document.
    Nets = "nets", nets {
        NAME "name" Str, WIDTH "width" U16, GLOBAL "global" U8, PIN_COUNT "pin_count" U32,
    }
    /// Instance parameters of the active document; `source` is default/override/computed.
    InstParams = "inst_params", inst_params {
        INST_ROW "inst_row" U32, NAME "name" Str, VALUE "value" Str, SOURCE "source" U8,
    }
    /// Elaborated paths of the active bench.
    Paths = "paths", paths {
        PATH "path" Str, CELL "cell" Str, IMPL "impl" Str, PARENT "parent" U32,
    }
    /// Hierarchical path to emitted name, from the last netlist.
    NameMap = "name_map", name_map {
        PATH "path" Str, EMITTED "emitted" Str,
    }
    /// Check violations of the active document.
    Violations = "violations", violations {
        CODE "code" Str, SEVERITY "severity" U8, OBJECT "object" U32, PARAM "param" Str,
    }
    /// Variables of the active cell.
    DocVars = "doc_vars", doc_vars {
        NAME "name" Str, EXPR "expr" Str, VALUE "value" Str,
    }
    /// Specs of the active bench.
    BenchSpecs = "bench_specs", bench_specs {
        NAME "name" Str, EXPR "expr" Str, MIN "min" Str, MAX "max" Str,
    }
    /// Outputs of the active bench.
    BenchOutputs = "bench_outputs", bench_outputs {
        NAME "name" Str, EXPR "expr" Str, MIN "min" Str, MAX "max" Str,
    }
    /// Results of finished jobs.
    Results = "results", results {
        JOB "job" U32, OUTPUT "output" Str, VALUE "value" Str, PASS "pass" U8,
    }
    /// Current selection.
    Selection = "selection", selection {
        DOC "doc" U32, ID "id" U32,
    }
    /// The composed UI tree.
    UiNodes = "ui_nodes", ui_nodes {
        ID "id" Str, KIND "kind" U8, PARENT "parent" Str, OWNER "owner" U16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_index_the_schema_and_names_are_unique() {
        assert_eq!(Table::COUNT, 15);
        for (i, &t) in Table::ALL.iter().enumerate() {
            assert_eq!(t as usize, i);
            assert_eq!(Table::from_raw(i as u32), Some(t));
            let cols = t.columns();
            assert!(!cols.is_empty());
            for (j, c) in cols.iter().enumerate() {
                assert!(cols[j + 1..].iter().all(|d| d.name != c.name), "{}", c.name);
            }
        }
        assert_eq!(Table::from_raw(Table::COUNT as u32), None);
        assert_eq!(
            Table::Nets.columns()[col::nets::PIN_COUNT as usize].name,
            "pin_count"
        );
        assert_eq!(
            Table::Wires.columns()[col::wires::NET as usize].ty,
            ColType::U32
        );
        assert_eq!(
            Table::UiNodes.columns()[col::ui_nodes::OWNER as usize].ty,
            ColType::U16
        );
    }

    #[test]
    fn scalars_round_trip_little_endian() {
        let mut out = Vec::new();
        0x1234_5678u32.put_le(&mut out);
        (-2i32).put_le(&mut out);
        0xBEEFu16.put_le(&mut out);
        assert_eq!(&out[..4], &[0x78, 0x56, 0x34, 0x12]);
        assert_eq!(u32::get_le(&out[..4]), 0x1234_5678);
        assert_eq!(i32::get_le(&out[4..8]), -2);
        assert_eq!(u16::get_le(&out[8..]), 0xBEEF);
        assert_eq!(<u16 as Scalar>::TY.width(), 2);
    }
}
