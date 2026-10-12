//! The SKILL file the plugin ships (spec `interop.md` §5), assembled from its parts in
//! `skill/` so each part can be reviewed and tested on its own.

/// The parts of `schemify.il`, in load order: (file name, text).
pub const PARTS: &[(&str, &str)] = &[
    ("sfy_io.il", include_str!("../skill/sfy_io.il")),
    ("sfy_cells.il", include_str!("../skill/sfy_cells.il")),
    ("sfy_ade.il", include_str!("../skill/sfy_ade.il")),
    ("sfy_retarget.il", include_str!("../skill/sfy_retarget.il")),
    ("sfy_selftest.il", include_str!("../skill/sfy_selftest.il")),
];

/// The probe a user runs once on a real Virtuoso (§5.5); not part of `schemify.il`.
pub const PROBE: &str = include_str!("../skill/sfy_probe.il");

/// `schemify.il`: every part, each preceded by a comment naming it.
pub fn schemify_il() -> String {
    let mut out = String::from(
        ";; schemify.il: Schemify <-> Virtuoso bridge (interop plugin). Load with\n;; (load \"/absolute/path/schemify.il\"); entry points: SfyExport SfyExportLib SfyImport SfyRetarget SfySelfTest.\n",
    );
    for (name, text) in PARTS {
        out.push_str(&format!("\n;; ---- {name}\n"));
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
    assert!(out.is_ascii(), "schemify.il must be ASCII");
    out
}
