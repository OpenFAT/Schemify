//! The guest libraries for other languages (`sdk/<lang>/`, docs/plugins/languages.md) carry
//! the ABI's numbers (version, error codes, table and column ids, levels, contribution
//! kinds) in a region between `BEGIN GENERATED` and `END GENERATED` lines. This test renders
//! that region from this crate's schema and fails when a library differs, so the ids exist
//! once (`schema.rs`, `lib.rs`). `SCHEMIFY_BLESS=1` rewrites the regions.

use schemify_abi::{ABI_VERSION, Code, Kind, Level, Table};

/// `inst_params` → `InstParams`.
fn camel(snake: &str) -> String {
    snake
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or(String::new(), |f| {
                format!("{}{}", f.to_ascii_uppercase(), c.as_str())
            })
        })
        .collect()
}

/// `inst_params` → `Inst_Params` (Odin's Ada_Case).
fn ada(snake: &str) -> String {
    snake.split('_').map(camel).collect::<Vec<_>>().join("_")
}

/// `E_DENIED` → `denied`.
fn code_snake(c: Code) -> String {
    c.name()[2..].to_ascii_lowercase()
}

fn c() -> String {
    let mut s = format!(
        "#define SCH_ABI_VERSION {ABI_VERSION}u\n\n/* Error codes (ABI §8); imports return them negated. */\nenum sch_code {{\n"
    );
    for c in Code::ALL {
        s += &format!("    SCH_{} = {},\n", c.name(), c as u32);
    }
    s += "};\nstatic const char *const sch_code_names[] = {\n    0,\n";
    for c in Code::ALL {
        s += &format!("    \"{}\",\n", c.name());
    }
    s += "};\n\n/* Snapshot tables (ABI §4). */\nenum sch_table {\n";
    for &t in Table::ALL {
        s += &format!(
            "    SCH_{} = {},\n",
            t.name().to_ascii_uppercase(),
            t as u32
        );
    }
    s += "};\n\n/* Column ids, SCH_<TABLE>_<COLUMN>, with their element types. */\nenum sch_column {\n";
    for &t in Table::ALL {
        for (i, col) in t.columns().iter().enumerate() {
            let name = format!("SCH_{}_{}", t.name(), col.name).to_ascii_uppercase();
            s += &format!("    {name} = {i}, /* {} */\n", col.ty.name());
        }
    }
    s += "};\n\n/* Log levels. */\nenum sch_level {\n";
    for &l in Level::ALL {
        s += &format!(
            "    SCH_{} = {},\n",
            l.name().to_ascii_uppercase(),
            l as u32
        );
    }
    s += "};\n\n/* Contribution kinds (ABI §1). */\n";
    for &k in Kind::ALL {
        s += &format!(
            "#define SCH_KIND_{} \"{}\"\n",
            k.name().to_ascii_uppercase(),
            k.name()
        );
    }
    s
}

fn zig() -> String {
    let mut s = format!(
        "pub const abi_version: u32 = {ABI_VERSION};\n\n/// Error codes (ABI §8); imports return them negated.\npub const Code = enum(u32) {{\n"
    );
    for c in Code::ALL {
        s += &format!("    {} = {},\n", code_snake(c), c as u32);
    }
    s += "};\n/// Wire names of `Code`, indexed by `code - 1`.\npub const code_names = [_][]const u8{\n";
    for c in Code::ALL {
        s += &format!("    \"{}\",\n", c.name());
    }
    s += "};\n\n/// Snapshot tables (ABI §4).\npub const Table = enum(u32) {\n";
    for &t in Table::ALL {
        s += &format!("    {},\n", t.name());
    }
    s += "};\n\n/// Column ids per table: `col.nets.pin_count`.\npub const col = struct {\n";
    for &t in Table::ALL {
        s += &format!("    pub const {} = struct {{\n", t.name());
        for (i, col) in t.columns().iter().enumerate() {
            s += &format!(
                "        pub const {}: u32 = {i}; // {}\n",
                col.name,
                col.ty.name()
            );
        }
        s += "    };\n";
    }
    s += "};\n\n/// Log levels.\npub const Level = enum(u32) {\n";
    for &l in Level::ALL {
        let n = l.name();
        s += &format!("    {},\n", if n == "error" { "@\"error\"" } else { n });
    }
    s += "};\n\n/// Contribution kinds (ABI §1); `@tagName` is the wire name.\npub const Kind = enum {\n";
    for &k in Kind::ALL {
        s += &format!("    {},\n", k.name());
    }
    s.push_str("};\n");
    s
}

fn odin() -> String {
    let mut s = format!(
        "ABI_VERSION :: {ABI_VERSION}\n\n// Error codes (ABI §8); imports return them negated.\nCode :: enum u32 {{\n"
    );
    for c in Code::ALL {
        s += &format!("\t{} = {},\n", ada(&code_snake(c)), c as u32);
    }
    s += "}\n// Wire names of `Code`.\nCODE_NAMES :: [Code]string {\n";
    for c in Code::ALL {
        s += &format!("\t.{} = \"{}\",\n", ada(&code_snake(c)), c.name());
    }
    s += "}\n\n// Snapshot tables (ABI §4).\nTable :: enum u32 {\n";
    for &t in Table::ALL {
        s += &format!("\t{},\n", ada(t.name()));
    }
    s += "}\n\n// Column ids per table: `u32(Nets_Col.Pin_Count)`.\n";
    for &t in Table::ALL {
        s += &format!("{}_Col :: enum u32 {{\n", ada(t.name()));
        for col in t.columns() {
            s += &format!("\t{}, // {}\n", ada(col.name), col.ty.name());
        }
        s += "}\n";
    }
    s += "\n// Log levels.\nLevel :: enum u32 {\n";
    for &l in Level::ALL {
        s += &format!("\t{},\n", ada(l.name()));
    }
    s += "}\n\n// Contribution kinds (ABI §1).\nKind :: enum {\n";
    for &k in Kind::ALL {
        s += &format!("\t{},\n", ada(k.name()));
    }
    s += "}\n// Wire names of `Kind`.\nKIND_NAMES :: [Kind]string {\n";
    for &k in Kind::ALL {
        s += &format!("\t.{} = \"{}\",\n", ada(k.name()), k.name());
    }
    s.push_str("}\n");
    s
}

fn go() -> String {
    let mut s = format!(
        "// AbiVersion is what schemify_abi returns.\nconst AbiVersion = {ABI_VERSION}\n\n// Code is an error code (ABI §8); imports return them negated.\ntype Code uint32\n\nconst (\n"
    );
    for c in Code::ALL {
        s += &format!("\tE{} Code = {}\n", camel(&code_snake(c)), c as u32);
    }
    s += ")\n\nvar codeNames = [...]string{\"\", ";
    s += &Code::ALL.map(|c| format!("\"{}\"", c.name())).join(", ");
    s += "}\n\n// Table is a snapshot table id (ABI §4).\ntype Table uint32\n\nconst (\n";
    for &t in Table::ALL {
        s += &format!("\t{} Table = {}\n", camel(t.name()), t as u32);
    }
    s += ")\n\n// Column ids, <Table><Column>.\nconst (\n";
    for &t in Table::ALL {
        for (i, col) in t.columns().iter().enumerate() {
            s += &format!(
                "\t{}{} uint32 = {i} // {}\n",
                camel(t.name()),
                camel(col.name),
                col.ty.name()
            );
        }
    }
    s += ")\n\n// Level is a log level.\ntype Level uint32\n\nconst (\n";
    for &l in Level::ALL {
        s += &format!("\tLevel{} Level = {}\n", camel(l.name()), l as u32);
    }
    s += ")\n\n// Kind is a contribution kind (ABI §1).\ntype Kind string\n\nconst (\n";
    for &k in Kind::ALL {
        s += &format!("\tKind{} Kind = \"{}\"\n", camel(k.name()), k.name());
    }
    s.push_str(")\n");
    s
}

fn assemblyscript() -> String {
    let mut s = format!(
        "export const ABI_VERSION: u32 = {ABI_VERSION};\n\n/** Error codes (ABI §8); imports return them negated. */\nexport enum Code {{\n"
    );
    for c in Code::ALL {
        s += &format!("  {} = {},\n", camel(&code_snake(c)), c as u32);
    }
    s += "}\n/** Wire names of `Code`, indexed by the code. */\nexport const CODE_NAMES: StaticArray<string> = [\"\", ";
    s += &Code::ALL.map(|c| format!("\"{}\"", c.name())).join(", ");
    s += "];\n\n/** Snapshot tables (ABI §4). */\nexport enum Table {\n";
    for &t in Table::ALL {
        s += &format!("  {} = {},\n", camel(t.name()), t as u32);
    }
    s += "}\n\n/** Column ids, <TABLE>_<COLUMN>. */\n";
    for &t in Table::ALL {
        for (i, col) in t.columns().iter().enumerate() {
            let name = format!("{}_{}", t.name(), col.name).to_ascii_uppercase();
            s += &format!("export const {name}: u32 = {i}; // {}\n", col.ty.name());
        }
    }
    s += "\n/** Log levels. */\nexport enum Level {\n";
    for &l in Level::ALL {
        s += &format!("  {} = {},\n", camel(l.name()), l as u32);
    }
    s += "}\n\n/** Contribution kinds (ABI §1). */\n";
    for &k in Kind::ALL {
        s += &format!(
            "export const KIND_{} = \"{}\";\n",
            k.name().to_ascii_uppercase(),
            k.name()
        );
    }
    s
}

/// A library's path under `sdk/` and the renderer of its generated region.
type Lib = (&'static str, fn() -> String);

const LIBS: [Lib; 5] = [
    ("c/schemify.h", c),
    ("zig/schemify.zig", zig),
    ("odin/schemify/schemify.odin", odin),
    ("go/schemify.go", go),
    ("assemblyscript/schemify.ts", assemblyscript),
];

#[test]
fn generated_regions_match_the_schema() {
    let bless = std::env::var_os("SCHEMIFY_BLESS").is_some();
    let mut stale = Vec::new();
    for (path, render) in LIBS {
        let full = format!("{}/sdk/{path}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("{full}: {e}"));
        let begin = text
            .find("BEGIN GENERATED")
            .expect("BEGIN GENERATED marker");
        let begin = begin + text[begin..].find('\n').expect("line end") + 1;
        let end = begin
            + text[begin..]
                .find("END GENERATED")
                .expect("END GENERATED marker");
        let end = text[..end].rfind('\n').map_or(end, |i| i + 1);
        let want = render();
        if text[begin..end] != want {
            if bless {
                let new = format!("{}{want}{}", &text[..begin], &text[end..]);
                std::fs::write(&full, new).unwrap();
            } else {
                stale.push(path);
            }
        }
    }
    assert!(
        stale.is_empty(),
        "stale generated regions (SCHEMIFY_BLESS=1 rewrites): {stale:?}"
    );
}
