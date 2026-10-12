//! Manufacturing-grid readers for layout tech files (D14): KLayout `.lyt` and Magic
//! `.tech`. Lengths are integer picometres, parsed exactly from the decimal text.
//!
//! - `.lyt`: the `<dbu>` and `<default-grids>` children of the root `<technology>`
//!   element. The grid marked `!` is KLayout's default grid, which PDKs set to the
//!   manufacturing grid (gf180 and IHP: `0.005!`). Nested `<dbu>`s (reader options) are
//!   ignored. Hand-parsed: one element level is all that is needed, so no XML crate.
//! - `.tech`: in the `cifoutput`/`cifinput` sections, each `style` with its
//!   `scalefactor <n> [nanometers|angstroms]` (default unit: centimicrons) and
//!   `gridlimit <n>` (in the same unit). The manufacturing grid is the first
//!   `cifoutput` style's `gridlimit`. `include` lines are listed by [`read_magic`] and
//!   spliced in, through a caller loader, by [`read_magic_with`].

use crate::decimal::parse_scaled;

/// Grid facts of a KLayout technology file, in picometres.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LytGrid {
    /// Database unit (`<dbu>`), when given.
    pub dbu_pm: Option<u32>,
    /// Editor grids (`<default-grids>`), in file order.
    pub grids_pm: Vec<u32>,
    /// The grid marked `!` in `<default-grids>`.
    pub default_grid_pm: Option<u32>,
}

/// Why a tech file was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TechError {
    /// The root element is not `<technology>`.
    NotTechnology,
    /// A length is not a decimal µm value with at most 6 fractional digits.
    BadLength {
        /// 1-based line of the value.
        line: u32,
        /// The offending text.
        text: String,
    },
}

/// Reads the grid facts of a KLayout `.lyt` file.
///
/// # Errors
///
/// [`TechError::NotTechnology`] when the first element is not `<technology>`;
/// [`TechError::BadLength`] when `dbu` or a grid is not an exact picometre count.
///
/// # Examples
///
/// ```
/// use interop::tech::read_lyt;
/// let g = read_lyt("<?xml version=\"1.0\"?>\n<technology><dbu>0.001</dbu>\
///                   <default-grids>0.005!,0.01</default-grids></technology>")?;
/// assert_eq!((g.dbu_pm, g.default_grid_pm), (Some(1_000), Some(5_000)));
/// # Ok::<(), interop::tech::TechError>(())
/// ```
pub fn read_lyt(text: &str) -> Result<LytGrid, TechError> {
    let b = text.as_bytes();
    let mut grid = LytGrid::default();
    let mut depth: u32 = 0;
    let mut root_seen = false;
    let mut i = 0;
    while let Some(off) = b[i..].iter().position(|c| *c == b'<') {
        i += off;
        let rest = &text[i..];
        let skip_to = |end: &str| rest.find(end).map_or(b.len(), |e| i + e + end.len());
        if rest.starts_with("<?") {
            i = skip_to("?>");
            continue;
        }
        if rest.starts_with("<!--") {
            i = skip_to("-->");
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &text[i + 1..i + gt];
        i += gt + 1;
        if tag.starts_with('!') {
            continue;
        }
        if let Some(_end) = tag.strip_prefix('/') {
            depth = depth.saturating_sub(1);
            continue;
        }
        let name = tag
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        if depth == 0 {
            if name != "technology" {
                return Err(TechError::NotTechnology);
            }
            root_seen = true;
        }
        if tag.ends_with('/') {
            continue;
        }
        depth += 1;
        if depth == 2 && (name == "dbu" || name == "default-grids") {
            let end = text[i..].find('<').map_or(text.len(), |e| i + e);
            let value = text[i..end].trim();
            let line = text[..i].bytes().filter(|c| *c == b'\n').count() as u32 + 1;
            if name == "dbu" {
                grid.dbu_pm = Some(micrometres_to_pm(value, line)?);
            } else {
                for item in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let marked = item.strip_suffix('!');
                    let pm = micrometres_to_pm(marked.unwrap_or(item), line)?;
                    grid.grids_pm.push(pm);
                    if marked.is_some() {
                        grid.default_grid_pm = Some(pm);
                    }
                }
            }
        }
    }
    if !root_seen {
        return Err(TechError::NotTechnology);
    }
    assert!(
        grid.default_grid_pm
            .is_none_or(|d| grid.grids_pm.contains(&d))
    );
    Ok(grid)
}

fn micrometres_to_pm(value: &str, line: u32) -> Result<u32, TechError> {
    parse_scaled(value, 6)
        .ok()
        .filter(|s| s.exact)
        .and_then(|s| u32::try_from(s.value).ok())
        .ok_or_else(|| TechError::BadLength {
            line,
            text: value.to_string(),
        })
}

/// Which CIF section a [`CifStyles`] row came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CifSection {
    /// `cifoutput` (GDS/CIF writing): its grid is the manufacturing grid.
    Output,
    /// `cifinput` (GDS/CIF reading).
    Input,
}

/// CIF styles of a Magic tech file (SoA, file order).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CifStyles {
    /// Section the style is in.
    pub section: Vec<CifSection>,
    /// Style name (first word after `style`).
    pub name: Vec<String>,
    /// Lambda (`scalefactor` × unit) in picometres, when given.
    pub lambda_pm: Vec<Option<u32>>,
    /// `gridlimit` × unit in picometres, when given.
    pub grid_pm: Vec<Option<u32>>,
    /// 1-based line of the `style`.
    pub line: Vec<u32>,
}

/// Grid facts of a Magic `.tech` file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MagicTech {
    /// CIF styles.
    pub styles: CifStyles,
    /// Files named by `include` lines: listed by [`read_magic`], spliced in (and listed)
    /// by [`read_magic_with`].
    pub includes: Vec<String>,
    /// `include` names [`read_magic_with`] could not load, or cut as a cycle or past
    /// [`MAGIC_INCLUDE_DEPTH`].
    pub unresolved: Vec<String>,
    /// Lines of `scalefactor`/`gridlimit` whose value is not a usable number.
    pub bad_lines: Vec<u32>,
}

impl MagicTech {
    /// Returns the manufacturing grid: the first `cifoutput` style's `gridlimit`.
    pub fn manufacturing_grid_pm(&self) -> Option<u32> {
        let s = &self.styles;
        (0..s.section.len())
            .find(|&i| s.section[i] == CifSection::Output)
            .and_then(|i| s.grid_pm[i])
    }
}

/// Reads the CIF styles and includes of a Magic `.tech` file. Total: unusable values are
/// listed in [`MagicTech::bad_lines`].
///
/// # Examples
///
/// ```
/// use interop::tech::read_magic;
/// let t = read_magic("cifoutput\nstyle gdsii\n scalefactor 50 nanometers\n gridlimit 5\nend\n");
/// assert_eq!(t.manufacturing_grid_pm(), Some(5_000));
/// assert_eq!(t.styles.lambda_pm[0], Some(50_000));
/// ```
pub fn read_magic(text: &str) -> MagicTech {
    let mut t = MagicTech::default();
    let mut section: Option<Option<CifSection>> = None; // Some(_) inside any section
    let mut unit_pm: u64 = 10_000;
    for (n, raw) in text.lines().enumerate() {
        let line = n as u32 + 1;
        let mut words = raw.split_whitespace();
        let Some(first) = words.next() else { continue };
        if first.starts_with('#') {
            continue;
        }
        match (section, first) {
            // Listed only; `read_magic_with` splices included files in before this runs.
            (None, "include") => t.includes.extend(words.next().map(str::to_string)),
            (None, s) => {
                section = Some(match s {
                    "cifoutput" => Some(CifSection::Output),
                    "cifinput" => Some(CifSection::Input),
                    _ => None,
                })
            }
            (Some(_), "end") => section = None,
            (Some(Some(sec)), "style") => {
                t.styles.section.push(sec);
                t.styles.name.push(words.next().unwrap_or("").to_string());
                t.styles.lambda_pm.push(None);
                t.styles.grid_pm.push(None);
                t.styles.line.push(line);
                unit_pm = 10_000;
            }
            (Some(Some(_)), "scalefactor" | "gridlimit") if !t.styles.name.is_empty() => {
                let value = words.next().unwrap_or("");
                if first == "scalefactor" {
                    unit_pm = match words.next() {
                        Some("nanometers") => 1_000,
                        Some("angstroms") => 100,
                        _ => 10_000,
                    };
                }
                let pm = parse_scaled(value, 0)
                    .ok()
                    .filter(|s| s.exact && s.value >= 0)
                    .and_then(|s| u32::try_from(s.value as u64 * unit_pm).ok());
                if pm.is_none() {
                    t.bad_lines.push(line);
                }
                let last = t.styles.name.len() - 1;
                if first == "scalefactor" {
                    t.styles.lambda_pm[last] = pm;
                } else {
                    t.styles.grid_pm[last] = pm;
                }
            }
            _ => {}
        }
    }
    assert!(
        t.styles.name.len() == t.styles.grid_pm.len() && t.styles.name.len() == t.styles.line.len()
    );
    t
}

/// Deepest chain of Magic `include`s [`read_magic_with`] follows.
pub const MAGIC_INCLUDE_DEPTH: u8 = 8;

/// Reads a Magic `.tech` file with every top-level `include <name>` replaced by the text
/// `load(name)` returns, as Magic does (IHP keeps its CIF sections in included files).
/// [`MagicTech::includes`] lists the names spliced in, in order; a name `load` cannot
/// provide, already in the include chain, or past [`MAGIC_INCLUDE_DEPTH`] is listed in
/// [`MagicTech::unresolved`] and skipped. Line numbers count the spliced text.
///
/// # Examples
///
/// ```
/// use interop::tech::read_magic_with;
/// let t = read_magic_with("tech\nend\ninclude cif\n", |n| {
///     (n == "cif").then(|| "cifoutput\nstyle gds\n scalefactor 10 nanometers\n gridlimit 5\nend\n".to_string())
/// });
/// assert_eq!((t.manufacturing_grid_pm(), t.includes.as_slice()), (Some(5_000), &["cif".to_string()][..]));
/// ```
pub fn read_magic_with(text: &str, mut load: impl FnMut(&str) -> Option<String>) -> MagicTech {
    let mut spliced = String::with_capacity(text.len());
    let (mut includes, mut unresolved) = (Vec::new(), Vec::new());
    splice(
        text,
        &mut load,
        &mut Vec::new(),
        &mut spliced,
        (&mut includes, &mut unresolved),
    );
    let mut t = read_magic(&spliced);
    t.includes = includes;
    t.unresolved = unresolved;
    assert!(
        t.includes
            .iter()
            .chain(&t.unresolved)
            .all(|n| !n.is_empty())
    );
    t
}

/// Appends `text` to `out` with its top-level `include` lines replaced (see
/// [`read_magic_with`]); `chain` holds the names being included.
fn splice(
    text: &str,
    load: &mut impl FnMut(&str) -> Option<String>,
    chain: &mut Vec<String>,
    out: &mut String,
    (includes, unresolved): (&mut Vec<String>, &mut Vec<String>),
) {
    assert!(chain.len() <= usize::from(MAGIC_INCLUDE_DEPTH));
    for raw in text.lines() {
        let mut words = raw.split_whitespace();
        let name = match (words.next(), words.next()) {
            (Some("include"), Some(name)) => name.to_owned(),
            _ => {
                out.push_str(raw);
                out.push('\n');
                continue;
            }
        };
        let body = (chain.len() < usize::from(MAGIC_INCLUDE_DEPTH) && !chain.contains(&name))
            .then(|| load(&name))
            .flatten();
        let Some(body) = body else {
            unresolved.push(name);
            continue;
        };
        includes.push(name.clone());
        chain.push(name);
        splice(&body, load, chain, out, (&mut *includes, &mut *unresolved));
        chain.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lyt_reads_only_root_children() {
        let src = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<technology>\n <name>x</name>\n <group/>\n <!-- <dbu>9</dbu> -->\n <dbu>0.001</dbu>\n <reader-options><gds2><dbu>0.5</dbu></gds2></reader-options>\n <default-grids>10.0,5.0,0.005!</default-grids>\n</technology>\n";
        let g = read_lyt(src).unwrap();
        assert_eq!(
            g,
            LytGrid {
                dbu_pm: Some(1000),
                grids_pm: vec![10_000_000, 5_000_000, 5000],
                default_grid_pm: Some(5000)
            }
        );
        assert_eq!(read_lyt("<layout/>"), Err(TechError::NotTechnology));
        assert_eq!(read_lyt(""), Err(TechError::NotTechnology));
        assert!(matches!(
            read_lyt("<technology><dbu>x</dbu></technology>"),
            Err(TechError::BadLength { line: 1, .. })
        ));
        let sky = read_lyt("<technology><dbu>0.001</dbu></technology>").unwrap();
        assert_eq!(sky.default_grid_pm, None);
    }

    #[test]
    fn magic_styles_units_and_includes() {
        let src = "tech\n format 35\nend\n# comment\ninclude ihp-cifout\ncifoutput\nstyle gdsii variants ()\n scalefactor 10  nanometers\n gridlimit 5\nstyle drc\n scalefactor 10\nend\ncifinput\nstyle imp\n scalefactor 1 angstroms\n gridlimit x\nend\ndrc\n style drc\n scalefactor 10\nend\n";
        let t = read_magic(src);
        assert_eq!(t.styles.name, ["gdsii", "drc", "imp"]);
        assert_eq!(t.styles.lambda_pm, [Some(10_000), Some(100_000), Some(100)]);
        assert_eq!(t.manufacturing_grid_pm(), Some(5_000));
        assert_eq!(t.includes, ["ihp-cifout"]);
        assert_eq!(t.bad_lines, [16]);
        assert_eq!(read_magic("").manufacturing_grid_pm(), None);
    }

    #[test]
    fn magic_includes_are_spliced_in_place() {
        let files = [
            ("a", "include b\ninclude missing\n"),
            (
                "b",
                "cifoutput\nstyle gds\n scalefactor 10 nanometers\n gridlimit 5\nend\ninclude a\n",
            ),
        ];
        let load = |n: &str| files.iter().find(|f| f.0 == n).map(|f| f.1.to_string());
        let t = read_magic_with("tech\nend\ninclude a\n", load);
        assert_eq!(t.manufacturing_grid_pm(), Some(5_000));
        assert_eq!(t.includes, ["a", "b"]);
        // `a` again (a cycle) and an unloadable name are reported, not followed.
        assert_eq!(t.unresolved, ["a", "missing"]);
        // Without a loader the include is only listed, and nothing is spliced.
        let plain = read_magic("include a\n");
        assert_eq!(
            (plain.includes.len(), plain.manufacturing_grid_pm()),
            (1, None)
        );
    }
}
