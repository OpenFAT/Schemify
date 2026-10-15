//! The bundled `core` library (D10, spec `import.md` §6): the cells under `devices/`,
//! compiled into the binary so `lib core builtin` needs no file system.
//!
//! The cells are authored for Schemify from SPICE semantics; only their pin *positions*
//! follow xschem's devices, because imported schematics connect by geometry.

macro_rules! core_files {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../../../devices/", $name)))),*]
    };
}

/// Every file of the core library as `(file name, bytes)`, sorted by name. The app
/// indexes it as library `core` without touching the file system (`lib core builtin`).
static CORE_FILES: &[(&str, &[u8])] = core_files![
    "ammeter.shprim",
    "bsource.shprim",
    "cap.shprim",
    "cccs.shprim",
    "ccvs.shprim",
    "diode.shprim",
    "gnd.shprim",
    "ind.shprim",
    "isource.shprim",
    "nmos4.shprim",
    "npn.shprim",
    "parax_cap.shprim",
    "pmos4.shprim",
    "pnp.shprim",
    "probe.shprim",
    "res.shprim",
    "switch.shprim",
    "tline.shprim",
    "vccs.shprim",
    "vcvs.shprim",
    "vdd.shprim",
    "vsource.shprim",
    "vswitch.shprim",
];

/// Returns the core library files as `(file name, bytes)`, sorted by file name: the
/// listing [`Project::index_library`](crate::Project::index_library) takes for `core`.
///
/// # Examples
///
/// ```
/// let core = schemify_doc::core_library();
/// assert!(core.iter().any(|(name, _)| *name == "res.shprim"));
/// ```
pub fn core_library() -> &'static [(&'static str, &'static [u8])] {
    debug_assert!(CORE_FILES.windows(2).all(|w| w[0].0 < w[1].0));
    CORE_FILES
}

/// Returns the bytes of core cell `cell` (file stem, `res` for `res.shprim`), or `None`
/// when the bundled library has no such cell.
///
/// Cost: a linear scan of the 23 bundled files.
pub fn core_file(cell: &str) -> Option<&'static [u8]> {
    debug_assert!(!cell.ends_with(".shprim"), "core_file takes a stem: {cell}");
    CORE_FILES
        .iter()
        .find(|(n, _)| n.strip_suffix(".shprim") == Some(cell))
        .map(|(_, b)| *b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_sorted_and_found_by_stem() {
        assert!(CORE_FILES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(core_file("res").is_some());
        assert!(core_file("lab_pin").is_none());
        for (name, bytes) in core_library() {
            let stem = name.strip_suffix(".shprim").expect("only .shprim files");
            assert_eq!(core_file(stem), Some(*bytes));
        }
    }
}
