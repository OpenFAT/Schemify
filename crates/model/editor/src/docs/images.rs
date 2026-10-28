//! Docs-page images (`![](x.png)` in prose): each path resolved under the project root,
//! read, size-checked and decoded once into the UI's image table.
//!
//! Contents:
//! - `pub fn load_images`
//! - `pub fn image_path`
//! - `pub fn load_image`
//! - `pub(super) fn png_size`

use std::path::{Path, PathBuf};

use schemify_doc::{Document, Loc, RecordKind, TableId};
use schemify_ui::{Image, TextSystem};

use crate::platform;

use super::{IMAGE_BYTES_MAX, IMAGE_SIDE_MAX, IMAGES_FAILED_MAX};

/// Registers in `text` every image the prose blocks of `doc` show (`![alt](path)` alone on a
/// line, [`schemify_ui::prose::image_line`]) that is not registered yet and not in `failed`:
/// [`load_image`] from the project at `root`. A path that fails joins `failed` (it shows its
/// alt text). Returns whether one loaded (the page needs a new layout) and the last failure.
///
/// Cost: one pass over the prose lines; file reads only for new paths.
///
/// # Panics
///
/// `failed` holds more than [`IMAGES_FAILED_MAX`] paths (it only grows here, capped).
pub fn load_images(
    doc: &Document,
    root: &Path,
    text: &mut TextSystem,
    failed: &mut Vec<String>,
) -> (bool, Option<String>) {
    assert!(failed.len() <= IMAGES_FAILED_MAX);
    let b = doc.doc_blocks();
    let (mut loaded, mut err) = (false, None);
    let rows = u32::try_from(b.kind.len()).expect("doc blocks < 2^12");
    for row in 0..rows {
        if b.kind[row as usize] != RecordKind::Prose {
            continue;
        }
        let rec = doc.record(Loc {
            table: TableId::DocBlocks,
            row,
        });
        for line in &rec.verbatim {
            let Some((_, path)) = schemify_ui::prose::image_line(line.trim()) else {
                continue;
            };
            if text.image_id(path).is_some() || failed.iter().any(|f| f == path) {
                continue;
            }
            match load_image(root, path) {
                Ok(img) => {
                    let registered = text.set_image(img);
                    assert_eq!(
                        registered,
                        text.image_id(path).is_some(),
                        "set_image's result"
                    );
                    loaded |= registered;
                    if !registered {
                        err = Some(format!("{path}: too many images on the page"));
                    }
                }
                Err(e) => err = Some(e),
            }
            if text.image_id(path).is_none() {
                if failed.len() >= IMAGES_FAILED_MAX {
                    failed.remove(0);
                }
                failed.push(path.to_owned());
            }
        }
    }
    assert!(failed.len() <= IMAGES_FAILED_MAX);
    (loaded, err)
}

/// The file of image `path` in the project at `root`: `path` must be project-relative with `/`
/// separators, without an empty, `.` or `..` segment, `\`, `:` or NUL (the plugin ABI's path
/// rule, plugin-abi §5), so prose cannot name a file outside the project.
///
/// # Errors
///
/// Any other path, with the reason.
///
/// # Panics
///
/// Never: the assert checks that the checked path stays under `root`.
pub fn image_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    let inside = path.len() <= 4096
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..");
    if !inside {
        return Err(format!("{path}: images must be inside the project"));
    }
    let file = root.join(path);
    assert!(
        file.starts_with(root),
        "a relative path without `..` stays under the root"
    );
    Ok(file)
}

/// Reads and decodes PNG image `path` of the project at `root` (premultiplied RGBA8).
///
/// # Errors
///
/// A path outside the project ([`image_path`]), an unreadable file, more than
/// [`IMAGE_BYTES_MAX`] bytes, not a PNG, a side of 0 or over [`IMAGE_SIDE_MAX`] (checked in the
/// header, before anything is allocated for the pixels), or a decoding error.
pub fn load_image(root: &Path, path: &str) -> Result<Image, String> {
    let bytes = platform::read(&image_path(root, path)?)?;
    if bytes.len() > IMAGE_BYTES_MAX {
        return Err(format!("{path}: over {} MiB", IMAGE_BYTES_MAX >> 20));
    }
    let (w, h) = png_size(&bytes).ok_or_else(|| format!("{path}: not a PNG image"))?;
    if w == 0 || h == 0 || w > IMAGE_SIDE_MAX || h > IMAGE_SIDE_MAX {
        return Err(format!(
            "{path}: {w}×{h} px (at most {IMAGE_SIDE_MAX} a side)"
        ));
    }
    let rgba = decode_png(&bytes, w, h).map_err(|e| format!("{path}: {e}"))?;
    Ok(Image::new(path, w, h, rgba))
}

/// Width and height from a PNG's signature and IHDR chunk; `None` when it is not one.
pub(super) fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if b.len() < 24 || b[..8] != SIGNATURE || b[12..16] != *b"IHDR" {
        return None;
    }
    let be = |at: usize| u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    Some((be(16), be(20)))
}

// ---- private ----

/// A `w × h` PNG as premultiplied RGBA8, the layout `ui::Image` holds: every color type and
/// bit depth normalized to 8-bit RGBA (palette expanded, transparency chunks applied, gray
/// widened), then each channel multiplied by alpha, truncated (as the renderer's own PNG decoder does,
/// so a page image draws the same pixels it did when render decoded it).
fn decode_png(bytes: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    // `load_image` checked the header, so the pixel count fits a u32 and the buffers.
    assert!((1..=IMAGE_SIDE_MAX).contains(&w), "width {w}");
    assert!((1..=IMAGE_SIDE_MAX).contains(&h), "height {h}");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("image too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if (info.width, info.height) != (w, h) {
        return Err(format!(
            "{}×{} px frame in a {w}×{h} px image",
            info.width, info.height
        ));
    }
    let px = (w * h) as usize;
    let mut rgba = Vec::with_capacity(px * 4);
    match info.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(&buf[..px * 4]),
        png::ColorType::GrayscaleAlpha => {
            for ga in buf[..px * 2].chunks_exact(2) {
                rgba.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }
        }
        // `ALPHA` adds an alpha channel to every color type, and `normalize_to_color8`
        // expands palettes: these cannot come out of the decoder.
        png::ColorType::Rgb | png::ColorType::Grayscale | png::ColorType::Indexed => {
            return Err(format!(
                "unexpected decoded color type {:?}",
                info.color_type
            ));
        }
    }
    for p in rgba.chunks_exact_mut(4) {
        let a = u16::from(p[3]);
        for c in &mut p[..3] {
            *c = (u16::from(*c) * a / 255) as u8;
        }
    }
    assert_eq!(rgba.len(), px * 4, "one RGBA pixel per image pixel");
    Ok(rgba)
}
