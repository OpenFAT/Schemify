//! SPICE model-card scanner for D14 gating: per-model legal L/W ranges from BSIM bin
//! parameters, `.subckt` wrappers, `.param`s, `.lib` sections, and Verilog-A (OSDI)
//! usage (research: `pdk-xschem-devices.md` §2.3, §3, §5.4).
//!
//! It reads; it does not simulate. Text rules (ngspice dialect):
//! - Names and keywords are case-insensitive; every identifier is stored lowercased.
//! - A line starting with `+` continues the previous logical line; comment lines in
//!   between do not break it. A `*` line is a comment; `;`, a `$` between blanks and a
//!   `//` after a blank start an inline comment.
//! - `.include f` reads `f` minus its section bodies; `.lib f s` reads only section `s` of
//!   `f`; `.lib s … .endl` defines a section. Relative paths resolve against the including
//!   file, then the search directories. Includes are followed to [`MAX_INCLUDE_DEPTH`],
//!   cycles are cut and reported, and a (file, section) pair is read once.
//! - `.if`/`.else`/`.endif` are not evaluated: every branch is scanned.
//! - OSDI evidence: `osdi`/`pre_osdi` commands, `N` device lines, and `.model` types that
//!   are not built into ngspice or XSPICE.

use std::path::{Component, Path, PathBuf};

use crate::{Span, push_span};

/// Deepest include chain followed; deeper ones are reported as [`ScanIssueKind::TooDeep`].
pub const MAX_INCLUDE_DEPTH: u8 = 16;
/// Most files one scan reads; more are reported as [`ScanIssueKind::TooManyFiles`].
pub const MAX_FILES: usize = 1 << 12;
/// [`Models::subckt`] value for a model defined outside any `.subckt`.
pub const TOP_LEVEL: u32 = u32::MAX;

/// What one scan reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScanOptions<'a> {
    /// Section of the entry file to read (`.lib entry section`); `None` reads the file
    /// the way `.include` would, skipping section bodies.
    pub section: Option<&'a str>,
    /// Directories tried, in order, for relative paths not found next to the includer
    /// (ngspice `sourcepath`).
    pub search_dirs: &'a [PathBuf],
}

/// `.model` cards. Arena: [`ModelScan::names`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Models {
    /// Model name, lowercased (BSIM bins carry a `.N` suffix; see [`base_name`]).
    pub name: Vec<Span>,
    /// Model type, lowercased (`nmos`, `r`, `psp103va`, …).
    pub kind: Vec<Span>,
    /// Row in [`ModelScan::subckts`] the card is defined inside, or [`TOP_LEVEL`].
    pub subckt: Vec<u32>,
    /// Row in [`ModelScan::files`].
    pub file: Vec<u16>,
    /// 1-based line of the `.model`.
    pub line: Vec<u32>,
}

/// Bin limits of the models that set any of `lmin lmax wmin wmax`, in metres as
/// written (no `.option scale` applied). Unset minima are 0, unset maxima +∞.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bins {
    /// Row in [`ModelScan::models`].
    pub model: Vec<u32>,
    /// `[lmin, lmax]`.
    pub l: Vec<[f64; 2]>,
    /// `[wmin, wmax]`.
    pub w: Vec<[f64; 2]>,
}

/// `.subckt` definitions. Arena: [`ModelScan::names`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Subckts {
    /// Subcircuit name, lowercased.
    pub name: Vec<Span>,
    /// Row in [`ModelScan::files`].
    pub file: Vec<u16>,
    /// 1-based line of the `.subckt`.
    pub line: Vec<u32>,
}

/// Device (instance) lines. Arena: [`ModelScan::names`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Devices {
    /// Enclosing subcircuit row, or [`TOP_LEVEL`].
    pub subckt: Vec<u32>,
    /// Device letter, lowercased (`m`, `n`, `r`, `x`, …).
    pub letter: Vec<u8>,
    /// `[first, count]` into [`ModelScan::device_words`]: the line's positional words
    /// after the instance name (nodes and model/subckt name; `k=v` pairs excluded).
    pub words: Vec<[u32; 2]>,
    /// Row in [`ModelScan::files`].
    pub file: Vec<u16>,
    /// 1-based line.
    pub line: Vec<u32>,
}

/// `.param` assignments. Arena: [`ModelScan::names`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params {
    /// Parameter name, lowercased.
    pub name: Vec<Span>,
    /// Value text as written (an expression is not evaluated).
    pub value: Vec<Span>,
    /// Row in [`ModelScan::files`].
    pub file: Vec<u16>,
    /// 1-based line.
    pub line: Vec<u32>,
}

/// A `.lib name` section definition or an `osdi`/`pre_osdi` path, with its location.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Located {
    /// Section name (lowercased) or OSDI path (as written, quotes removed).
    pub text: Vec<Span>,
    /// Row in [`ModelScan::files`].
    pub file: Vec<u16>,
    /// 1-based line.
    pub line: Vec<u32>,
}

/// Why part of the library was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanIssueKind {
    /// An include/lib path no loader call returned (detail: the path as written).
    Unresolved,
    /// A file includes itself through the chain (detail: the path).
    Cycle,
    /// The chain exceeds [`MAX_INCLUDE_DEPTH`] (detail: the path).
    TooDeep,
    /// More than [`MAX_FILES`] files (detail: the path).
    TooManyFiles,
    /// `.lib file section` named a section the file does not define (detail: section).
    SectionNotFound,
    /// A bin limit is an expression, not a number (detail: the value).
    NonNumericBin,
    /// `.ends` with no open `.subckt`.
    UnbalancedEnds,
    /// A `.model` line without name and type.
    MalformedModel,
}

/// One reported problem. Arena for `detail`: [`ModelScan::names`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanIssue {
    /// Row in [`ModelScan::files`] of the file containing the offending line.
    pub file: u16,
    /// 1-based line; 0 when the problem is the entry file itself (no including line).
    pub line: u32,
    /// What went wrong.
    pub kind: ScanIssueKind,
    /// Context text.
    pub detail: Span,
}

/// Classification of every logical line read, for count conservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LineKind {
    /// Blank or `*` comment line.
    Comment,
    /// `.model`.
    Model,
    /// `.subckt`.
    Subckt,
    /// `.ends`.
    Ends,
    /// `.param`.
    Param,
    /// `.include` / `.inc`.
    Include,
    /// `.lib file section`.
    LibCall,
    /// `.lib section` (definition start).
    LibSection,
    /// `.endl`.
    Endl,
    /// `.control` … `.endc` and the commands inside.
    Control,
    /// `osdi` / `pre_osdi` command.
    Osdi,
    /// `.if` / `.elseif` / `.else` / `.endif` (not evaluated).
    Conditional,
    /// Any other dot command (`.option`, `.temp`, …).
    OtherDirective,
    /// A device line.
    Device,
    /// A line inside a section that is not being read.
    Inactive,
    /// Anything else (stray `+`, junk).
    Other,
}

/// Number of [`LineKind`] variants.
pub const LINE_KINDS: usize = LineKind::Other as usize + 1;

/// Everything one scan found, as tables over all files read.
///
/// Invariant (asserted on exit of [`scan`]): `line_counts` sums to `lines_read`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelScan {
    /// Files read, in first-read order (normalised paths). Rows are `u16` handles.
    pub files: Vec<PathBuf>,
    /// Arena for every [`Span`] in this scan.
    pub names: String,
    /// `.model` cards.
    pub models: Models,
    /// Bin limits (models that set any).
    pub bins: Bins,
    /// `.subckt` definitions.
    pub subckts: Subckts,
    /// Device lines.
    pub devices: Devices,
    /// Positional words of device lines, lowercased.
    pub device_words: Vec<Span>,
    /// `.param` assignments.
    pub params: Params,
    /// `.lib` section definitions seen.
    pub sections: Located,
    /// `osdi` / `pre_osdi` loads.
    pub osdi_loads: Located,
    /// Problems, with locations.
    pub issues: Vec<ScanIssue>,
    /// Logical lines read across all files.
    pub lines_read: u32,
    /// Logical lines per [`LineKind`].
    pub line_counts: [u32; LINE_KINDS],
}

/// How a `.model` type is implemented in ngspice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backing {
    /// A built-in SPICE device model.
    Builtin,
    /// An XSPICE code model.
    Xspice,
    /// A Verilog-A module loaded through OSDI (an `N` device instantiates it).
    Osdi,
    /// Not built in and no `N` device uses it: needs review.
    Unknown,
}

/// One bin rectangle in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bin {
    /// `[lmin, lmax]`.
    pub l: [f64; 2],
    /// `[wmin, wmax]`.
    pub w: [f64; 2],
}

/// ngspice built-in model types.
const BUILTIN: &[&str] = &[
    "r", "c", "l", "sw", "csw", "urc", "ltra", "d", "npn", "pnp", "njf", "pjf", "nmos", "pmos",
    "nmf", "pmf", "nhfet", "phfet", "vdmos", "txl", "cpl", "res", "cap", "ind",
];
/// XSPICE analog code models (digital ones all start with `d_`).
const XSPICE: &[&str] = &[
    "adc_bridge",
    "dac_bridge",
    "gain",
    "summer",
    "mult",
    "divide",
    "limit",
    "ilimit",
    "climit",
    "pwl",
    "filesource",
    "sine",
    "triangle",
    "square",
    "aswitch",
    "zener",
    "hyst",
    "core",
    "lcouple",
    "cmeter",
    "lmeter",
    "int",
    "s_xfer",
    "slew",
    "oneshot",
    "potentiometer",
    "table2d",
    "table3d",
    "sidiode",
    "delay",
];

/// Scans the library rooted at `entry`, reading files through `load`.
///
/// `load` gets candidate paths (lexically normalised) and returns the file text, or
/// `None` when it does not exist; it is the only IO. Never fails: unreadable includes,
/// cycles and malformed cards are reported in [`ModelScan::issues`].
///
/// # Time complexity
///
/// O(total bytes) plus O(files²) for the read-once check (`files` ≤ [`MAX_FILES`]).
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use schemify_import::spice::{scan, ScanOptions};
/// let lib = ".lib tt\n.subckt nfet d g s b\nm1 d g s b nfet__model l=1 w=1\n\
///            .model nfet__model.0 nmos lmin=1e-6 lmax=2e-6\n+ wmin=1e-6 wmax=5e-6\n.ends\n.endl tt\n";
/// let s = scan(Path::new("m.lib"), &ScanOptions { section: Some("tt"), ..Default::default() },
///              |_| Some(lib.to_string()));
/// assert!(s.issues.is_empty());
/// assert_eq!(s.bins_of("nfet")[0].w, [1e-6, 5e-6]);
/// ```
pub fn scan<F: FnMut(&Path) -> Option<String>>(
    entry: &Path,
    opts: &ScanOptions,
    mut load: F,
) -> ModelScan {
    let mut s = ModelScan::default();
    let mut st = State {
        stack: Vec::new(),
        visited: Vec::new(),
    };
    let section = opts.section.map(str::to_ascii_lowercase);
    s.read_file(&mut st, &mut load, opts, None, entry, section.as_deref(), 0);
    let counted: u32 = s.line_counts.iter().sum();
    assert_eq!(counted, s.lines_read, "line conservation");
    assert!(st.stack.is_empty() && s.files.len() <= MAX_FILES);
    s
}

struct State {
    /// (file, section) pairs currently being read, outermost first.
    stack: Vec<(PathBuf, Option<String>)>,
    /// Every (file, section) pair read so far.
    visited: Vec<(PathBuf, Option<String>)>,
}

/// Where the include came from, for issue locations.
#[derive(Clone, Copy)]
struct Origin {
    file: u16,
    line: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    Outside,
    Target,
    Other,
}

impl ModelScan {
    #[allow(clippy::too_many_arguments)]
    fn read_file<F: FnMut(&Path) -> Option<String>>(
        &mut self,
        st: &mut State,
        load: &mut F,
        opts: &ScanOptions,
        origin: Option<Origin>,
        raw_path: &Path,
        section: Option<&str>,
        depth: u8,
    ) {
        let at = origin.unwrap_or(Origin { file: 0, line: 0 });
        let shown = raw_path.to_string_lossy().into_owned();
        if depth > MAX_INCLUDE_DEPTH {
            self.issue(at, ScanIssueKind::TooDeep, &shown);
            return;
        }
        let base = origin.map(|o| {
            self.files[o.file as usize]
                .parent()
                .unwrap_or(Path::new(""))
                .to_path_buf()
        });
        let mut candidates = Vec::new();
        if raw_path.is_absolute() || base.is_none() {
            candidates.push(normalize(raw_path));
        } else {
            candidates.push(normalize(&base.unwrap_or_default().join(raw_path)));
            candidates.extend(
                opts.search_dirs
                    .iter()
                    .map(|d| normalize(&d.join(raw_path))),
            );
        }
        let Some((path, text)) = candidates
            .into_iter()
            .find_map(|p| load(&p).map(|t| (p, t)))
        else {
            self.issue(at, ScanIssueKind::Unresolved, &shown);
            return;
        };
        let key = (path.clone(), section.map(str::to_string));
        if st.stack.contains(&key) {
            self.issue(at, ScanIssueKind::Cycle, &shown);
            return;
        }
        if st.visited.contains(&key) {
            return; // ponytail: linear set, ≤ MAX_FILES entries; a HashSet when libraries grow
        }
        let file = match self.files.iter().position(|f| *f == path) {
            Some(i) => i as u16,
            None if self.files.len() == MAX_FILES => {
                self.issue(at, ScanIssueKind::TooManyFiles, &shown);
                return;
            }
            None => {
                self.files.push(path);
                (self.files.len() - 1) as u16
            }
        };
        st.visited.push(key.clone());
        st.stack.push(key);
        let found = self.read_lines(st, load, opts, file, &text, section, depth);
        st.stack.pop();
        if let (Some(name), false) = (section, found) {
            self.issue(at, ScanIssueKind::SectionNotFound, name);
        }
    }

    /// Reads one file's logical lines; returns whether `section` (if any) was found.
    #[allow(clippy::too_many_arguments)]
    fn read_lines<F: FnMut(&Path) -> Option<String>>(
        &mut self,
        st: &mut State,
        load: &mut F,
        opts: &ScanOptions,
        file: u16,
        text: &str,
        section: Option<&str>,
        depth: u8,
    ) -> bool {
        let mut cx = FileState {
            file,
            section,
            region: Region::Outside,
            found: false,
            control: false,
            subckts: Vec::new(),
        };
        let mut pending: Option<(u32, String)> = None;
        for (i, physical) in text.lines().enumerate() {
            let number = i as u32 + 1;
            let content = strip_comment(physical);
            let trimmed = content.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('*') {
                self.count(LineKind::Comment);
            } else if let Some(rest) = trimmed.strip_prefix('+') {
                match &mut pending {
                    Some((_, l)) => {
                        l.push(' ');
                        l.push_str(rest);
                    }
                    None => self.count(LineKind::Other),
                }
            } else {
                if let Some((n, l)) = pending.take() {
                    self.logical_line(st, load, opts, &mut cx, n, &l, depth);
                }
                pending = Some((number, content.to_string()));
            }
        }
        if let Some((n, l)) = pending.take() {
            self.logical_line(st, load, opts, &mut cx, n, &l, depth);
        }
        cx.found
    }

    #[allow(clippy::too_many_arguments)]
    fn logical_line<F: FnMut(&Path) -> Option<String>>(
        &mut self,
        st: &mut State,
        load: &mut F,
        opts: &ScanOptions,
        cx: &mut FileState,
        line: u32,
        text: &str,
        depth: u8,
    ) {
        let words = words(text);
        let first = words
            .first()
            .map(|w| w.to_ascii_lowercase())
            .unwrap_or_default();
        let at = Origin {
            file: cx.file,
            line,
        };
        // Section structure is tracked even while inactive.
        if first == ".lib" && words.len() == 2 {
            let name = words[1].to_ascii_lowercase();
            self.sections.text.push(push_span(&mut self.names, &name));
            self.sections.file.push(cx.file);
            self.sections.line.push(line);
            if cx.region == Region::Outside {
                cx.region = if cx.section == Some(name.as_str()) {
                    Region::Target
                } else {
                    Region::Other
                };
                cx.found |= cx.region == Region::Target;
            }
            return self.count(LineKind::LibSection);
        }
        if first == ".endl" {
            cx.region = Region::Outside;
            return self.count(LineKind::Endl);
        }
        let active = match cx.section {
            Some(_) => cx.region == Region::Target,
            None => cx.region == Region::Outside,
        };
        if !active {
            return self.count(LineKind::Inactive);
        }
        let subckt = cx.subckts.last().copied().unwrap_or(TOP_LEVEL);
        match first.as_str() {
            ".control" => {
                cx.control = true;
                self.count(LineKind::Control)
            }
            ".endc" => {
                cx.control = false;
                self.count(LineKind::Control)
            }
            "osdi" | "pre_osdi" => {
                let path = words.get(1).map_or("", |w| unquote(w));
                self.osdi_loads.text.push(push_span(&mut self.names, path));
                self.osdi_loads.file.push(cx.file);
                self.osdi_loads.line.push(line);
                self.count(LineKind::Osdi)
            }
            _ if cx.control => self.count(LineKind::Control),
            ".include" | ".inc" => {
                self.count(LineKind::Include);
                if let Some(p) = words.get(1) {
                    self.read_file(
                        st,
                        load,
                        opts,
                        Some(at),
                        Path::new(unquote(p)),
                        None,
                        depth + 1,
                    );
                }
            }
            ".lib" => {
                self.count(LineKind::LibCall);
                let section = words.get(2).map(|w| w.to_ascii_lowercase());
                if let Some(p) = words.get(1) {
                    self.read_file(
                        st,
                        load,
                        opts,
                        Some(at),
                        Path::new(unquote(p)),
                        section.as_deref(),
                        depth + 1,
                    );
                }
            }
            ".model" => {
                self.count(LineKind::Model);
                // Parameter lists may be parenthesised: flatten the parens away.
                self.model(
                    cx.file,
                    line,
                    subckt,
                    &self::words(&text.replace(['(', ')'], " ")),
                );
            }
            ".subckt" => {
                self.count(LineKind::Subckt);
                let name = words
                    .get(1)
                    .map_or(String::new(), |w| w.to_ascii_lowercase());
                self.subckts.name.push(push_span(&mut self.names, &name));
                self.subckts.file.push(cx.file);
                self.subckts.line.push(line);
                cx.subckts.push(self.subckts.name.len() as u32 - 1);
            }
            ".ends" => {
                self.count(LineKind::Ends);
                if cx.subckts.pop().is_none() {
                    self.issue(at, ScanIssueKind::UnbalancedEnds, "");
                }
            }
            ".param" => {
                self.count(LineKind::Param);
                for (k, v) in pairs(&words[1..]) {
                    self.params
                        .name
                        .push(push_span(&mut self.names, &k.to_ascii_lowercase()));
                    self.params.value.push(push_span(&mut self.names, v));
                    self.params.file.push(cx.file);
                    self.params.line.push(line);
                }
            }
            // ponytail: every branch is scanned, so bins of all branches union; evaluate
            // `.if` with the expr crate if a PDK ever gates bins on parameters.
            ".if" | ".elseif" | ".else" | ".endif" => self.count(LineKind::Conditional),
            f if f.starts_with('.') => self.count(LineKind::OtherDirective),
            f if f.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) => {
                self.count(LineKind::Device);
                let first_word = self.device_words.len() as u32;
                let mut i = 1;
                while i < words.len() {
                    if words.get(i + 1) == Some(&"=") {
                        i += 3;
                        continue;
                    }
                    let w = words[i].to_ascii_lowercase();
                    if w == "params:" {
                        break;
                    }
                    self.device_words.push(push_span(&mut self.names, &w));
                    i += 1;
                }
                self.devices.subckt.push(subckt);
                self.devices.letter.push(f.as_bytes()[0]);
                self.devices
                    .words
                    .push([first_word, self.device_words.len() as u32 - first_word]);
                self.devices.file.push(cx.file);
                self.devices.line.push(line);
            }
            _ => self.count(LineKind::Other),
        }
    }

    fn model(&mut self, file: u16, line: u32, subckt: u32, words: &[&str]) {
        // `.model name type (k=v …)`, `.model name type(k=v …)`.
        let (Some(name), Some(kind)) = (words.get(1), words.get(2)) else {
            return self.issue(Origin { file, line }, ScanIssueKind::MalformedModel, "");
        };
        let kind = kind.to_ascii_lowercase();
        let row = self.models.name.len() as u32;
        self.models
            .name
            .push(push_span(&mut self.names, &name.to_ascii_lowercase()));
        self.models.kind.push(push_span(&mut self.names, &kind));
        self.models.subckt.push(subckt);
        self.models.file.push(file);
        self.models.line.push(line);
        let mut limits = [None; 4];
        for (k, v) in pairs(&words[2..]) {
            let k = k.to_ascii_lowercase();
            let Some(slot) = ["lmin", "lmax", "wmin", "wmax"]
                .iter()
                .position(|n| *n == k)
            else {
                continue;
            };
            match parse_number(v) {
                Some(x) => limits[slot] = Some(x),
                None => self.issue(Origin { file, line }, ScanIssueKind::NonNumericBin, v),
            }
        }
        if limits.iter().any(Option::is_some) {
            let [lmin, lmax, wmin, wmax] = limits;
            self.bins.model.push(row);
            self.bins
                .l
                .push([lmin.unwrap_or(0.0), lmax.unwrap_or(f64::INFINITY)]);
            self.bins
                .w
                .push([wmin.unwrap_or(0.0), wmax.unwrap_or(f64::INFINITY)]);
        }
    }

    fn count(&mut self, kind: LineKind) {
        self.lines_read += 1;
        self.line_counts[kind as usize] += 1;
    }

    fn issue(&mut self, at: Origin, kind: ScanIssueKind, detail: &str) {
        let detail = push_span(&mut self.names, detail);
        self.issues.push(ScanIssue {
            file: at.file,
            line: at.line,
            kind,
            detail,
        });
    }

    /// Returns `span`'s text from [`ModelScan::names`].
    pub fn str(&self, span: Span) -> &str {
        span.of(&self.names)
    }

    /// Returns how model row `m`'s type is implemented.
    pub fn backing(&self, m: usize) -> Backing {
        let kind = self.str(self.models.kind[m]);
        if BUILTIN.contains(&kind) {
            return Backing::Builtin;
        }
        if kind.starts_with("d_") || XSPICE.contains(&kind) {
            return Backing::Xspice;
        }
        let name = self.str(self.models.name[m]);
        let used_by_n = (0..self.devices.letter.len()).any(|d| {
            self.devices.letter[d].eq_ignore_ascii_case(&b'n') && self.words(d).any(|w| w == name)
        });
        if used_by_n {
            Backing::Osdi
        } else {
            Backing::Unknown
        }
    }

    fn words(&self, d: usize) -> impl Iterator<Item = &str> {
        let [first, count] = self.devices.words[d];
        self.device_words[first as usize..(first + count) as usize]
            .iter()
            .map(|s| self.str(*s))
    }

    /// Returns the model rows a model or subcircuit name stands for: models named `name`
    /// or whose [`base_name`] is `name`, or, for a subcircuit, the models defined inside
    /// it plus those its devices reference. Sorted, without duplicates.
    ///
    /// # Time complexity
    ///
    /// O(models × (1 + words of the subcircuit's devices)); call once per symbol.
    pub fn models_of(&self, name: &str) -> Vec<usize> {
        let name = name.to_ascii_lowercase();
        let mut out = Vec::new();
        self.push_models_named(&name, &mut out);
        for s in (0..self.subckts.name.len()).filter(|&s| self.str(self.subckts.name[s]) == name) {
            out.extend((0..self.models.name.len()).filter(|&m| self.models.subckt[m] == s as u32));
            for d in (0..self.devices.subckt.len()).filter(|&d| self.devices.subckt[d] == s as u32)
            {
                for w in self.words(d) {
                    self.push_models_named(w, &mut out);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn push_models_named(&self, name: &str, out: &mut Vec<usize>) {
        out.extend((0..self.models.name.len()).filter(|&m| {
            let model = self.str(self.models.name[m]);
            model == name || base_name(model) == name
        }));
    }

    /// Returns the bin rectangles of every model [`ModelScan::models_of`] `name` finds.
    /// Empty means the models carry no bin limits (no L/W gating from model cards).
    pub fn bins_of(&self, name: &str) -> Vec<Bin> {
        let models = self.models_of(name);
        (0..self.bins.model.len())
            .filter(|&b| models.binary_search(&(self.bins.model[b] as usize)).is_ok())
            .map(|b| Bin {
                l: self.bins.l[b],
                w: self.bins.w[b],
            })
            .collect()
    }

    /// Returns true when subcircuit `name` (or a model of that name) is Verilog-A backed:
    /// it contains an `N` device or references an [`Backing::Osdi`] model.
    pub fn uses_osdi(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        let n_device = (0..self.subckts.name.len())
            .filter(|&s| self.str(self.subckts.name[s]) == lower)
            .any(|s| {
                (0..self.devices.subckt.len())
                    .any(|d| self.devices.subckt[d] == s as u32 && self.devices.letter[d] == b'n')
            });
        n_device
            || self
                .models_of(name)
                .into_iter()
                .any(|m| self.backing(m) == Backing::Osdi)
    }
}

struct FileState<'s> {
    file: u16,
    section: Option<&'s str>,
    region: Region,
    found: bool,
    control: bool,
    subckts: Vec<u32>,
}

/// Returns a BSIM bin model's family name: `name` minus a trailing `.<digits>`.
pub fn base_name(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => name,
    }
}

/// Merges bin rectangles into disjoint, sorted `[min, max]` intervals per axis (bins that
/// touch or overlap merge). This is the per-axis envelope: L and W are each legal
/// somewhere, but a pair (L, W) is legal only if [`contains`] says so.
pub fn union(bins: &[Bin]) -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
    fn merge(mut v: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
        v.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
        let mut out: Vec<[f64; 2]> = Vec::with_capacity(v.len());
        for r in v {
            match out.last_mut() {
                // Relative slack absorbs bins written with different digit counts.
                Some(last) if r[0] <= last[1] + last[1].abs() * 1e-9 => last[1] = last[1].max(r[1]),
                _ => out.push(r),
            }
        }
        out
    }
    (
        merge(bins.iter().map(|b| b.l).collect()),
        merge(bins.iter().map(|b| b.w).collect()),
    )
}

/// Returns true when some bin holds (`l`, `w`), limits inclusive.
pub fn contains(bins: &[Bin], l: f64, w: f64) -> bool {
    bins.iter()
        .any(|b| b.l[0] <= l && l <= b.l[1] && b.w[0] <= w && w <= b.w[1])
}

/// Parses a SPICE number: a decimal with optional exponent, then an optional scale
/// suffix (`t g meg k m u n p f a mil`, any case) and ignored unit letters.
/// Returns `None` for expressions (`{…}`, `'…'`) and anything else.
///
/// # Examples
///
/// ```
/// use schemify_import::spice::parse_number;
/// assert_eq!(parse_number("0.15u"), Some(0.15e-6));
/// assert_eq!(parse_number("10Meg"), Some(10e6));
/// assert_eq!(parse_number("2.0e-05"), Some(2.0e-5));
/// assert_eq!(parse_number("{l*2}"), None);
/// ```
pub fn parse_number(text: &str) -> Option<f64> {
    let b = text.as_bytes();
    let mut i = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let digits = |i: &mut usize| {
        let s = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > s
    };
    let mut any = digits(&mut i);
    if b.get(i) == Some(&b'.') {
        i += 1;
        any |= digits(&mut i);
    }
    if !any {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if digits(&mut j) {
            i = j;
        }
    }
    let mantissa: f64 = text[..i].parse().ok()?;
    let suffix = text[i..].to_ascii_lowercase();
    if !suffix.bytes().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let scale = if suffix.starts_with("meg") {
        1e6
    } else if suffix.starts_with("mil") {
        25.4e-6
    } else {
        match suffix.bytes().next() {
            None => 1.0,
            Some(b't') => 1e12,
            Some(b'g') => 1e9,
            Some(b'k') => 1e3,
            Some(b'm') => 1e-3,
            Some(b'u') => 1e-6,
            Some(b'n') => 1e-9,
            Some(b'p') => 1e-12,
            Some(b'f') => 1e-15,
            Some(b'a') => 1e-18,
            Some(_) => 1.0, // unit letters only (`1V`)
        }
    };
    Some(mantissa * scale)
}

/// Cuts a physical line at its inline comment (`;`, a blank-delimited `$`, ` //`).
fn strip_comment(line: &str) -> &str {
    let b = line.as_bytes();
    for i in 0..b.len() {
        let blank_before = i == 0 || b[i - 1] == b' ' || b[i - 1] == b'\t';
        let cut = match b[i] {
            b';' => true,
            b'$' => blank_before && b.get(i + 1).is_none_or(|c| *c == b' ' || *c == b'\t'),
            b'/' => blank_before && b.get(i + 1) == Some(&b'/'),
            _ => false,
        };
        if cut {
            return &line[..i];
        }
    }
    line
}

/// Splits a logical line into words: whitespace-separated, `=` a word of its own,
/// `{…}` `(…)` `'…'` `"…"` kept whole.
fn words(line: &str) -> Vec<&str> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == b.len() {
            break;
        }
        if b[i] == b'=' {
            out.push("=");
            i += 1;
            continue;
        }
        let start = i;
        let (mut depth, mut quote) = (0u32, None);
        while i < b.len() {
            let c = b[i];
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None => match c {
                    b'\'' | b'"' => quote = Some(c),
                    b'{' | b'(' => depth += 1,
                    b'}' | b')' => depth = depth.saturating_sub(1),
                    b'=' if depth == 0 => break,
                    c if c.is_ascii_whitespace() && depth == 0 => break,
                    _ => {}
                },
            }
            i += 1;
        }
        out.push(&line[start..i]);
    }
    out
}

/// Returns the `key = value` pairs of a word list.
fn pairs<'a>(words: &[&'a str]) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < words.len() {
        if words[i + 1] == "=" && i + 2 < words.len() {
            out.push((words[i], words[i + 2]));
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

fn unquote(w: &str) -> &str {
    let t = w.trim_matches('"');
    t.strip_prefix('\'')
        .and_then(|t| t.strip_suffix('\''))
        .unwrap_or(t)
}

/// Lexically normalises `p`: drops `.` components and folds `x/..`.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn fs(files: &[(&str, &str)]) -> impl FnMut(&Path) -> Option<String> + use<> {
        let map: BTreeMap<PathBuf, String> = files
            .iter()
            .map(|(p, t)| (PathBuf::from(p), t.to_string()))
            .collect();
        move |p: &Path| map.get(p).cloned()
    }

    #[test]
    fn lib_sections_includes_and_bins() {
        // Shape of sky130: .lib tt → corner file → pm3 file with a subckt holding binned models.
        let files = [
            (
                "lib/top.lib",
                "* top\n.lib tt\n.include \"../cells/n__tt.corner.spice\"\n.endl tt\n.lib ff\n.include missing.spice\n.endl ff\n",
            ),
            (
                "cells/n__tt.corner.spice",
                ".include n.pm3.spice ; inline comment\n.param tox = 4.1e-9 sky_x = {tox*2}\n",
            ),
            (
                "cells/n.pm3.spice",
                ".subckt  sky_nfet d g s b\n+ mult=1\nmsky_nfet d g s b sky_nfet__model l={l} w={w}\n\
                 .model sky_nfet__model.0 nmos\n* comment inside continuation\n+ lmin = 1.5e-07 lmax = 1.0e-06 wmin = 4.2e-07 wmax = 1e-06\n\
                 .model sky_nfet__model.1 nmos lmin=1.0e-06 lmax=2e-06 wmin=1e-06 wmax=7e-06\n.ends sky_nfet\n",
            ),
        ];
        let s = scan(
            Path::new("lib/top.lib"),
            &ScanOptions {
                section: Some("TT"),
                ..Default::default()
            },
            fs(&files),
        );
        assert!(s.issues.is_empty(), "{:?}", s.issues);
        assert_eq!(s.files.len(), 3);
        let bins = s.bins_of("SKY_NFET");
        assert_eq!(bins.len(), 2);
        let (l, w) = union(&bins);
        assert_eq!((l, w), (vec![[1.5e-7, 2e-6]], vec![[4.2e-7, 7e-6]]));
        assert!(contains(&bins, 1.5e-6, 5e-6) && !contains(&bins, 2e-7, 5e-6));
        assert_eq!(s.params.name.len(), 2);
        assert_eq!(s.str(s.params.value[1]), "{tox*2}");
        assert_eq!(s.backing(0), Backing::Builtin);
        assert!(!s.uses_osdi("sky_nfet"));
        // Section ff was skipped (inactive), so its missing include is never reported.
        assert!(s.line_counts[LineKind::Inactive as usize] > 0);
    }

    #[test]
    fn osdi_wrappers_cycles_and_missing_things() {
        let files = [
            (
                "m/corner.lib",
                ".lib mos_tt\n.lib corner.lib mos_tt\n.include mod.lib\n.lib nope.lib x\n.endl\n.lib other.lib zz\n",
            ),
            (
                "m/mod.lib",
                ".subckt lv_nmos d g s b w=1u\nNlv d g s b lv_psp\n.ends\n.model lv_psp psp103va type=+1\n.model r1 r3_cmc\n.ends\n",
            ),
            ("m/other.lib", ".lib aa\n.endl\n"),
            (
                "m/.spiceinit",
                "osdi '$PDK_ROOT/osdi/psp103.osdi'\n.control\npre_osdi x.osdi\n.endc\n",
            ),
        ];
        let s = scan(
            Path::new("m/corner.lib"),
            &ScanOptions {
                section: Some("mos_tt"),
                ..Default::default()
            },
            fs(&files),
        );
        let kinds: Vec<_> = s
            .issues
            .iter()
            .map(|i| (i.line, i.kind, s.str(i.detail).to_string()))
            .collect();
        assert_eq!(
            kinds,
            [
                (2, ScanIssueKind::Cycle, "corner.lib".into()),
                (6, ScanIssueKind::UnbalancedEnds, String::new()),
                (4, ScanIssueKind::Unresolved, "nope.lib".into()),
            ]
        );
        assert!(s.uses_osdi("LV_NMOS"));
        assert_eq!(s.backing(0), Backing::Osdi);
        assert_eq!(s.backing(1), Backing::Unknown);
        // A .lib call into a file that lacks the section.
        let s = scan(
            Path::new("m/other.lib"),
            &ScanOptions {
                section: Some("zz"),
                ..Default::default()
            },
            fs(&files),
        );
        assert_eq!(s.issues[0].kind, ScanIssueKind::SectionNotFound);
        let s = scan(
            Path::new("m/.spiceinit"),
            &ScanOptions::default(),
            fs(&files),
        );
        assert_eq!(
            (s.osdi_loads.text.len(), s.str(s.osdi_loads.text[0])),
            (2, "$PDK_ROOT/osdi/psp103.osdi")
        );
    }

    #[test]
    fn numbers_words_comments() {
        assert_eq!(parse_number("1mil"), Some(25.4e-6));
        assert_eq!(parse_number("5"), Some(5.0));
        assert_eq!(parse_number("1e"), Some(1.0));
        assert_eq!(parse_number("-.5k"), Some(-500.0));
        assert_eq!(parse_number("1.5*2"), None);
        assert_eq!(parse_number("'1u'"), None);
        assert_eq!(
            words("a b={x + y} c = 'p q' d(e f)"),
            ["a", "b", "=", "{x + y}", "c", "=", "'p q'", "d(e f)"]
        );
        assert_eq!(strip_comment("r1 a b 1k $ note"), "r1 a b 1k ");
        assert_eq!(strip_comment(".include $PDK/x"), ".include $PDK/x");
        assert_eq!(base_name("m.12"), "m");
        assert_eq!(base_name("m.x1"), "m.x1");
        assert_eq!(normalize(Path::new("a/./b/../c")), PathBuf::from("a/c"));
    }

    #[test]
    fn depth_bound() {
        let s = scan(Path::new("a"), &ScanOptions::default(), |p: &Path| {
            Some(format!(".include {}x\n", p.display()))
        });
        assert_eq!(s.issues.len(), 1);
        assert_eq!(s.issues[0].kind, ScanIssueKind::TooDeep);
        assert_eq!(s.files.len(), MAX_INCLUDE_DEPTH as usize + 1);
    }
}
