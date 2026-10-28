//! More of the state contribution: menus and dialogs of the parity commands, panels, text tabs.

use schemify_ui::{Kind, StyleProp, records};

use crate::docs::quote as q;
use crate::session::TEXT_WINDOW;
use crate::{checks, docs, spines};

use super::canvas::{LAYER_NAMES, USER_LAYERS};
use super::dialog::Dialog;
use super::{Editor, Panel};

impl Editor {
    /// The parity part of the state contribution: menu check marks, the recent-files menu,
    /// the context menu, the layers and recovery dialogs, canvas-only view, toolbar.
    pub(super) fn contribute_parity(&self, t: &mut String) {
        use std::fmt::Write as _;
        let o = &self.opts;
        let flags = [
            ("options.auto_join_trim_wires", o.auto_trim),
            ("options.color_postscript_svg", o.export_color),
            ("options.transparent_svg_background", o.export_transparent),
            ("options.crosshair.draw_crosshair", o.crosshair),
            ("options.crosshair.draw_snap_cursor", o.snap_cursor),
            ("options.enable_stretch", o.stretch),
            ("options.netlist_format_symbol_mode.flat_netlist", o.flat),
            ("options.persistent_wire_line_place_command", o.persistent),
            ("view.show_hide.draw_grid_axes", o.grid_axes),
            ("view.show_hide.show_toolbar", o.toolbar),
            ("view.show_hide.symbol_text", o.symbol_text),
            (
                "view.show_hide.show_erc_info_window",
                self.panel == super::Panel::Checks,
            ),
            ("view.toggle_variable_line_width", o.variable_width),
            ("highlight.auto_highlight_net_pins", o.auto_highlight),
            (
                "highlight.enable_highlight_connected_instances",
                o.highlight_insts,
            ),
            ("highlight.increment_hilight_color", o.incr_color),
            ("highlight.view_only_probes", o.only_probes),
            (
                "highlight.compare_schematics",
                self.compare.as_ref().is_some_and(|c| c.on),
            ),
            (
                "simulation.graphs.auto_highlight_plotted_nets",
                o.highlight_plotted,
            ),
            (
                "simulation.graphs.annotate_operating_point_into_schematic",
                self.op_on,
            ),
            (
                "symbol.show_symbols.show_symbol_details",
                o.show_symbols == 0,
            ),
            (
                "symbol.show_symbols.show_instance_bounding_boxes_for_subcirc",
                o.show_symbols == 1,
            ),
            (
                "symbol.show_symbols.show_instance_bounding_boxes_for_all_sym",
                o.show_symbols == 2,
            ),
            (
                "options.netlist_format_symbol_mode.spice_netlist",
                self.dialect == schemify_elab::Dialect::Ngspice,
            ),
            (
                "options.netlist_format_symbol_mode.espice_netlist",
                self.dialect == schemify_elab::Dialect::Espice,
            ),
            (
                "options.netlist_format_symbol_mode.verilog_netlist",
                self.dialect == schemify_elab::Dialect::Verilog,
            ),
        ];
        for (id, on) in flags {
            let _ = writeln!(t, "set {id} checked {}", u8::from(on));
        }
        let _ = writeln!(
            t,
            "set status.mode label {}",
            spines::dialect_word(self.dialect)
        );
        let _ = writeln!(t, "set status.stretch label {}", u8::from(o.stretch));
        // Open recent: one row per remembered file.
        if self.recent.is_empty() {
            let _ = writeln!(
                t,
                "node file.open_recent.none item\n  parent menu.file.open_recent\n  label \"(none)\"\n  disabled 1"
            );
        }
        for (i, p) in self.recent.iter().enumerate() {
            let name = p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let _ = writeln!(
                t,
                "node file.open_recent.{i} item\n  parent menu.file.open_recent\n  label {}\n  command file.open_recent.{i}\n  tooltip {}",
                q(&name),
                q(&p.display().to_string())
            );
        }
        if o.fullscreen {
            for id in ["menubar", "toolbar", "tabs", "status"] {
                let _ = writeln!(t, "remove {id}");
            }
        } else if !o.toolbar {
            let _ = writeln!(t, "remove toolbar");
        }
        match &self.dialog {
            Dialog::Ctx(selected) => {
                let rows: &[(&str, &str)] = if *selected {
                    &[
                        ("Abort command", "view.redraw"),
                        ("Edit attributes", "properties.edit"),
                        ("Edit attr in editor", "properties.edit_with_editor"),
                        ("Descend schematic", "edit.push_schematic"),
                        ("Descend symbol", "edit.push_symbol"),
                        ("Delete selection", "edit.delete"),
                        ("Rotate selection", "edit.rotate_selected_objects"),
                        ("Flip selection", "edit.horizontal_flip_selected_objects"),
                        (
                            "Rotate in-place sel.",
                            "edit.rotate_in_place_selected_objects",
                        ),
                        (
                            "Flip in-place sel.",
                            "edit.horizontal_flip_in_place_selected_object",
                        ),
                        ("Cut selection", "edit.cut"),
                        ("Copy selection", "edit.copy"),
                        ("Move selection", "edit.move_objects"),
                        ("Duplicate selection", "edit.duplicate_objects"),
                    ]
                } else {
                    &[
                        ("Abort command", "view.redraw"),
                        ("Open most recent", "file.open_most_recent"),
                        ("Edit attributes", "properties.edit"),
                        ("Edit attr in editor", "properties.edit_with_editor"),
                        ("Go to upper level", "edit.pop"),
                        ("Insert symbol", "tools.insert_symbol"),
                        ("Insert wire", "tools.insert_wire"),
                        ("Insert line", "tools.insert_line"),
                        ("Insert box", "tools.insert_rect"),
                        ("Insert polygon", "tools.insert_polygon"),
                        ("Insert arc", "tools.insert_arc"),
                        ("Insert circle", "tools.insert_circle"),
                        ("Insert text", "tools.insert_text"),
                        ("Paste clipboard", "edit.paste"),
                    ]
                };
                for (i, (label, cmd)) in rows.iter().enumerate() {
                    let _ = writeln!(
                        t,
                        "node dialog.ctx.row.{i} item\n  parent dialog.ctx\n  label {}\n  command {cmd}\n  style list-row",
                        q(label)
                    );
                }
            }
            Dialog::Layers => {
                for l in 1..USER_LAYERS {
                    let name = LAYER_NAMES.get(usize::from(l)).copied().unwrap_or("");
                    let label = if name.is_empty() {
                        format!("{l:02}")
                    } else {
                        format!("{l:02}-{name}")
                    };
                    let _ = writeln!(
                        t,
                        "node dialog.layers.row.{l} item\n  parent dialog.layers.list\n  label {}\n  command layers.toggle.{l}\n  checked {}\n  style list-row",
                        q(&label),
                        u8::from(o.hidden & (1 << l) == 0)
                    );
                }
            }
            Dialog::File(text) => {
                let _ = writeln!(t, "set dialog.edit_file.text value {}", q_multiline(text));
                let _ = writeln!(t, "set dialog.edit_file.text input field.typed");
            }
            Dialog::Recover(d, n) => {
                let name = self.session.name(*d);
                let _ = writeln!(
                    t,
                    "set dialog.recover.title label {}",
                    q(&format!(
                        "{name}: {n} unsaved edit(s) from a crash can be recovered"
                    ))
                );
            }
            _ => {}
        }
    }
    /// Appends the text tab's `shown` text to `t` as one fenced-code `textview.body` prose
    /// node under `textview`. A long text is windowed by the tab's scroll offset: only
    /// [`TEXT_WINDOW`] lines (or two screens, if more) are in the node, with spacers above and
    /// below sized in whole code rows ([`schemify_ui::prose::code_metrics`]), so the frame
    /// lays out and paints a screenful, not 16k lines.
    ///
    /// The visible rows are the ones the unwindowed node would show only if every line above
    /// the window is exactly one row: printable ASCII, no fence, and narrower than the wrap
    /// width the last layout had (less any shrink of the window since). The window therefore
    /// never starts past the first line that is not, nor before the tab has been laid out once.
    /// Lines below the window are counted as one row each; the scroll extent is exact once
    /// the window reaches the end.
    pub(super) fn contribute_text_body(&self, t: &mut String, shown: &str) {
        use std::fmt::Write as _;
        let sh = &self.shell;
        let node = |t: &mut String, body: &str| {
            let fenced = format!("```\n{body}\n```");
            let _ = writeln!(
                t,
                "node textview.body prose\n  parent textview\n  value {}\n  style text-view-body",
                docs::quote(&fenced)
            );
        };
        let style = sh
            .strings
            .find("text-view-body")
            .map_or(schemify_ui::NO_STYLE, |s| sh.tables.styles.row(s));
        let font = sh
            .tables
            .styles
            .get(style, Kind::Prose, StyleProp::Font)
            .max(8);
        let Some(m) = u16::try_from(font)
            .ok()
            .and_then(|px| schemify_ui::prose::code_metrics(&sh.text, px))
        else {
            return node(t, shown);
        };
        // The lines the prose parser sees between the fences.
        let lines: Vec<&str> = shown
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect();
        let n = lines.len() as u32;
        assert!(n >= 1);
        let row = |id: &str| {
            let i = sh.tables.nodes.find(sh.strings.find(id)?)?;
            sh.layout.row_of(i).map(|r| sh.layout.rect()[r])
        };
        let scroll = sh
            .strings
            .find("textview")
            .map_or(0.0, |s| sh.ui.scroll_of(s));
        let len = TEXT_WINDOW.max(2 * (self.viewport.1 / m.line).ceil() as u32);
        let (mut start, end) = records::window(scroll, m.line, n, len);
        // Wrap width: last frame's, less the window's shrink since (the tab never shrinks more).
        let shrink = sh
            .layout
            .rect()
            .first()
            .map_or(0.0, |r| r.w - self.viewport.0)
            .max(0.0);
        let wrap = row("textview.body").map_or(0.0, |r| r.w - 2.0 * m.pad - shrink);
        let cols = (wrap / m.advance - 0.01).floor().max(0.0) as usize;
        let one_row = |l: &str| {
            l.len() <= cols
                && l.bytes().all(|b| (b' '..=b'~').contains(&b))
                && !l.trim_start().starts_with("```")
        };
        let clean = lines[..start as usize]
            .iter()
            .take_while(|l| one_row(l))
            .count() as u32;
        start = start.min(clean);
        if start == 0 && end == n {
            return node(t, shown);
        }
        let px = m.line as u32;
        records::spacer(t, "textview", "textview.gap-top", start * px, 0);
        node(t, &lines[start as usize..end as usize].join("\n"));
        records::spacer(t, "textview", "textview.gap-bottom", (n - end) * px, 0);
        debug_assert!(start < end);
    }
    /// Projects the bottom panel, the text tab and the integration state (menu checks) into
    /// the state contribution `t`.
    pub(super) fn contribute_integration(&self, t: &mut String) {
        use std::fmt::Write as _;
        match self.panel {
            Panel::None => {
                let _ = writeln!(t, "remove bottom");
            }
            Panel::Checks => {
                let cell = self.cell.as_ref();
                checks::contribute(t, cell, &self.blocked);
            }
            Panel::Results => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let list = self.shell.strings.find("results.list");
                    let scroll = list.map_or(0.0, |s| self.shell.ui.scroll_of(s));
                    self.sim.contribute(t, scroll);
                }
            }
        }
        // Several staged edits: step through them on the docs page's banner.
        if self.staged.is_some()
            && self.docs_shown()
            && self
                .active()
                .is_some_and(|(h, _)| self.session.project.stages(h).len() > 1)
        {
            for (id, label, cmd) in [
                ("docs.staged.prev", "<", "edit.show_staged.prev"),
                ("docs.staged.next", ">", "edit.show_staged.next"),
            ] {
                let _ = writeln!(
                    t,
                    "node {id} button\n  parent docs.staged\n  label {label}\n  command {cmd}"
                );
            }
        }
        if let Some(v) = self.session.text_view() {
            let _ = writeln!(t, "remove canvas");
            // The pane is a column, so the scroll's content height never reaches the `work`
            // row and the tab scrolls instead of growing to its text (as `docs.pane` does).
            let _ = writeln!(
                t,
                "style text-pane\n  grow 1\n  dir column\nnode textview.pane panel\n  parent work\n  style text-pane\nnode textview scroll\n  parent textview.pane\n  style text-view"
            );
            let _ = writeln!(
                t,
                "node textview.find text_input\n  parent textview\n  value {}\n  command text.find\n  tooltip {}",
                docs::quote(&v.find),
                docs::quote("Find (Enter): the file's lines containing the text, numbered")
            );
            let shown = match &v.found {
                Some((count, lines)) => {
                    let _ = writeln!(
                        t,
                        "node textview.found label\n  parent textview\n  label {}",
                        docs::quote(&format!("{count} line(s) contain {:?}", v.find))
                    );
                    lines
                }
                None => &v.body,
            };
            self.contribute_text_body(t, shown);
        }
        for (word, d, _) in spines::DIALECTS {
            let _ = writeln!(
                t,
                "set sim.dialect.{word} checked {}",
                u8::from(d == self.dialect)
            );
        }
        let _ = writeln!(
            t,
            "set simulation.show_netlist_after_netlist_command checked {}",
            u8::from(self.show_netlist)
        );
        let _ = writeln!(t, "set waves.op_annotate checked {}", u8::from(self.op_on));
    }
}

/// Quotes a multi-line value: like `q` but keeps line breaks as `\n` escapes.
pub(super) fn q_multiline(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
