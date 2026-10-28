//! Read-only text tabs: netlists, find, changelog, packages, processes, job logs, help.
//!
//! Contents:
//! - `pub(super) const TEXT_TAB_LINES`
//! - `pub(super) const TEXT_TAB_BYTES`
//! - `pub(super) const CHANGELOG_ROWS`
//! - `Editor::find_in_text`
//! - `Editor::netlist`
//! - `Editor::text_tab`
//! - `Editor::changelog`
//! - `Editor::package_status`
//! - `Editor::list_processes`
//! - `Editor::job_log`
//! - `pub(super) fn expand_bus`
//! - `pub(super) fn help_text`

use schemify_doc::DocHandle;
use schemify_elab::{Request, Severity};
use schemify_expr::Value;

use crate::session::TextView;
use crate::{platform, spines};

use super::highlight::env_refs;
use super::{Editor, Panel};

/// Netlist lines a text tab shows (the prose widget's source cap); the file has them all.
pub(super) const TEXT_TAB_LINES: usize = 1 << 14;

/// Bytes of a netlist a text tab shows (the file has all of it).
pub(super) const TEXT_TAB_BYTES: usize = 256 << 10;

/// Rows of hierarchy the changelog lists at most.
pub(super) const CHANGELOG_ROWS: usize = 1 << 12;

impl Editor {
    /// `text.find`: the lines of the text tab's file (all of it, not only the part the tab
    /// shows; the tab's text when there is no file) containing `query`, ASCII
    /// case-insensitively, numbered, at most a tab's worth shown; an empty query shows the
    /// text again.
    pub(super) fn find_in_text(&mut self, query: &str) {
        use std::fmt::Write as _;
        let q = query.trim().to_owned();
        let Some(v) = self.session.text_view_mut() else {
            self.session.say("find: no text tab");
            return;
        };
        v.find.clone_from(&q);
        if q.is_empty() {
            v.found = None;
            return;
        }
        let full = platform::read(&v.path)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_else(|| v.body.clone());
        let needle = q.to_ascii_lowercase();
        let (mut lines, mut count) = (String::new(), 0u32);
        for (i, l) in full.lines().enumerate() {
            if l.to_ascii_lowercase().contains(&needle) {
                count += 1;
                if count as usize <= TEXT_TAB_LINES {
                    let _ = writeln!(lines, "{:>6}: {l}", i + 1);
                }
            }
        }
        v.found = Some((count, lines));
        self.session.say(format!("find {q:?}: {count} line(s)"));
    }
    /// `n` (`whole`: the tab's top cell) / `Shift+N` (the cell at this level): elaborate,
    /// gate on errors (D13: the blocking lines go to the checks panel), emit in the selected
    /// dialect, write `.schemify/netlist/<cell>.<ext>`, and show it in a text tab when "show
    /// netlist" is on.
    pub(super) fn netlist(&mut self, whole: bool) {
        let Some(tab) = self.session.tabs().get(self.session.active()) else {
            return;
        };
        let h = if whole {
            tab.stack[0].doc
        } else {
            tab.stack.last().expect("tabs are never empty").doc
        };
        let Some(stem) = self
            .session
            .path(h)
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
        else {
            return;
        };
        let dialect = self.dialect;
        let env = env_refs(&self.env);
        let req = Request {
            config: None,
            corner: None,
            overrides: &[],
            env: &env,
            dialect,
        };
        let host = &mut self.host;
        let mut funcs = |name: &str, args: &[Value<'_>]| host.call_fn(name, args);
        let e = match spines::elaborate_all(&mut self.session, h, &req, &mut funcs) {
            Ok(e) => e,
            Err(err) => {
                self.session.say(format!("netlist: {err}"));
                return;
            }
        };
        if e.violations.blocking() {
            self.blocked = e
                .violations
                .rows
                .iter()
                .filter(|v| v.code.severity() == Severity::Error)
                .map(|v| spines::violation_line(&e.names, v))
                .collect();
            self.panel = Panel::Checks;
            self.session.say(format!(
                "netlist blocked: {} error(s) (see the checks panel)",
                self.blocked.len()
            ));
            return;
        }
        self.blocked.clear();
        // Options → Flat netlist: the hierarchy expanded (SPICE dialects only).
        let spice = matches!(
            dialect,
            schemify_elab::Dialect::Ngspice
                | schemify_elab::Dialect::Xyce
                | schemify_elab::Dialect::Cdl
        );
        let text = if self.opts.flat && spice {
            schemify_netlist::emit_flat(&e, dialect)
        } else {
            schemify_netlist::emit(&e, dialect).text
        };
        let path = spines::netlist_file(&self.session.root, &stem, dialect);
        if let Err(err) = platform::write_atomic(&path, text.as_bytes()) {
            self.session.say(format!("netlist: {err}"));
            return;
        }
        let lines = text.lines().count();
        self.session.say(format!(
            "netlist ({}) written: {} ({lines} lines)",
            spines::dialect_word(dialect),
            path.display()
        ));
        if self.show_netlist {
            let mut body: String = text
                .lines()
                .take(TEXT_TAB_LINES)
                .collect::<Vec<_>>()
                .join("\n");
            if body.len() > TEXT_TAB_BYTES {
                let mut cut = TEXT_TAB_BYTES;
                while !body.is_char_boundary(cut) {
                    cut -= 1;
                }
                body.truncate(cut);
            }
            if body.len() < text.len().saturating_sub(1) {
                body.push_str(&format!(
                    "\n… (truncated; the whole netlist is in {})",
                    path.display()
                ));
            }
            let title = path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            self.session.open_text_tab(TextView {
                title: format!("{title} (netlist)"),
                path,
                body,
                find: String::new(),
                found: None,
            });
            self.shell.invalidate();
        }
    }
    /// Shows `body` in a read-only text tab titled `title` (`key` identifies it, so asking
    /// again refreshes the same tab).
    pub(super) fn text_tab(&mut self, title: &str, key: &str, body: String) {
        self.session.open_text_tab(TextView {
            title: title.to_owned(),
            path: self.session.root.join(".schemify").join("text").join(key),
            body,
            find: String::new(),
            found: None,
        });
    }
    pub(super) fn changelog(&mut self, h: DocHandle) {
        let docs = self.hierarchy_docs(h);
        let mut body = String::from("Files of the hierarchy (modified, seconds since 1970)\n\n");
        for d in docs.into_iter().take(CHANGELOG_ROWS) {
            let Some(p) = self.session.path(d) else {
                continue;
            };
            let t = platform::mtime_unix_s(p).map_or_else(|| "-".to_owned(), |s| s.to_string());
            let dirty = if self.session.project.is_dirty(d) {
                " *"
            } else {
                ""
            };
            body.push_str(&format!("{t:>12}  {}{dirty}\n", p.display()));
        }
        self.text_tab("changelog", "changelog", body);
    }
    /// Tools → Package status: what `schemify pack status` prints for the user config
    /// (config spec §5), in a text tab.
    pub(super) fn package_status(&mut self) {
        use schemify_cli::pack;
        let config = self.host.config();
        let specs = &config.table().packs;
        let body = match pack::Paths::of(config) {
            Err(e) => format!("user packages: {e}\n"),
            Ok(_) if specs.is_empty() => {
                "no user packages (`schemify.pack.add` in init.lua)\n".to_owned()
            }
            Ok(paths) => {
                let report = pack::status(specs, &paths);
                let mut b = report.lines.join("\n");
                b.push('\n');
                if report.failed > 0 {
                    b.push_str(&format!(
                        "\n{} package(s) missing or broken: run `schemify pack sync`\n",
                        report.failed
                    ));
                }
                b
            }
        };
        assert!(body.ends_with('\n'));
        self.text_tab("packages", "packages", body);
    }
    pub(super) fn list_processes(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut body = String::from("Simulation jobs\n\n");
            match self.sim.tables() {
                Some((_, jobs, _)) => {
                    for j in 0..jobs.len() {
                        body.push_str(&format!("job {j}: {:?}\n", jobs.state(j)));
                    }
                    body.push('\n');
                    body.push_str(&self.sim.progress());
                    body.push('\n');
                }
                None => body.push_str("no run in this session\n"),
            }
            self.text_tab("processes", "processes", body);
        }
    }
    /// Simulation → View last job data / errors: the selected job's `stdout.log` /
    /// `stderr.log`.
    pub(super) fn job_log(&mut self, errors: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(dir) = self.sim.dir() else {
                self.session.say("no run in this session");
                return;
            };
            let name = if errors {
                schemify_run::STDERR
            } else {
                schemify_run::STDOUT
            };
            let path = dir.join(self.sim.selected.to_string()).join(name);
            let body = platform::read(&path)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_else(|e| e);
            let title = if errors { "job errors" } else { "job output" };
            self.text_tab(title, title, body);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = errors;
    }
}

/// Every bit of a bus net key `a[3:0]` (`a[3] … a[0]`); other keys as they are.
pub(super) fn expand_bus(key: &str) -> Vec<String> {
    let parsed = key.strip_suffix(']').and_then(|k| {
        let (base, range) = k.rsplit_once('[')?;
        let (hi, lo) = range.split_once(':')?;
        Some((base, hi.parse::<i64>().ok()?, lo.parse::<i64>().ok()?))
    });
    match parsed {
        Some((base, hi, lo)) if (hi - lo).abs() < 1 << 16 => {
            let step = if hi >= lo { -1 } else { 1 };
            let mut out = Vec::new();
            let mut i = hi;
            loop {
                out.push(format!("{base}[{i}]"));
                if i == lo {
                    break;
                }
                i += step;
            }
            out
        }
        _ => vec![key.to_owned()],
    }
}

/// The Help text tab: the key summary and where the manuals are.
pub(super) fn help_text() -> String {
    "Schemify help\n\n\
     Keys are xschem's (D25); `/` toggles a key summary over the canvas.\n\n\
     Editing: w wire, l line, r rect, p polygon, Shift+C arc, Ctrl+Shift+C circle, t text,\n\
     Ins insert symbol, m move, Ctrl+M stretch move, Shift+M kissing move, c duplicate,\n\
     Del delete, Shift+R rotate, Shift+F flip, u / Shift+U undo / redo, Ctrl+F search,\n\
     & join/trim wires, Alt+U align to grid, right click: context menu.\n\n\
     View: f zoom full, z zoom box, Shift+Z / Ctrl+Z zoom in / out, % grid, Alt+X crosshair.\n\n\
     Hierarchy: e / Ctrl+E descend / return, i symbol, Alt+E / Alt+I in a new tab.\n\n\
     Files: Ctrl+S save, Ctrl+Shift+S save as, Ctrl+Shift+O most recent, B merge.\n\n\
     Specs: docs/spec/ (file format, expressions, netlisting); decisions: docs/dev/decisions.md.\n"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buses_expand_per_bit() {
        assert_eq!(expand_bus("d[2:0]"), ["d[2]", "d[1]", "d[0]"]);
        assert_eq!(expand_bus("d[0:1]"), ["d[0]", "d[1]"]);
        assert_eq!(expand_bus("vin"), ["vin"]);
    }
}
