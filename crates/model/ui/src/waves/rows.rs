//! The waveform window's rows (spec waves §3–§5): the contribution that follows the viewer
//! tables, layered on [`super::WAVES_UI`]. A projection: reads `Waves` and the window's
//! transient state, writes only interned strings.
//!
//! Contents: `WaveUi::contribute(waves, strings) -> Contribution`

use schemify_wave::{LineStyle, PaneMode, Radix, TRACE_COLORS, Waves};

use super::{ROW_H, WaveUi};
use crate::kinds::{Kind, Prop, StyleProp};
use crate::strings::{Strings, Sym};
use crate::tables::{Contribution, Edit};

impl WaveUi {
    /// The rows that follow the viewer tables: signal rows (filtered, scrolled), one graph node
    /// per pane, trace rows, toggle states, field values, the open dialog's fields and status.
    /// Interns every string it emits into `strings` (bounded by datasets' variable names and
    /// user input).
    ///
    /// # Panics
    ///
    /// If the rows take more than [`Contribution::CAPACITY`] edits: the signal list is
    /// scrolled, so only a bug grows it that far.
    pub fn contribute(&self, waves: &Waves, strings: &mut Strings) -> Contribution {
        debug_assert!(
            self.signal_rows <= super::ROWS_MAX,
            "signal rows are bounded by set_capacity"
        );
        let mut e = Vec::with_capacity(256);
        let mut s = |t: &str| strings.intern(t);
        let node = |e: &mut Vec<Edit>, id: Sym, kind: Kind, parent: Sym| {
            e.push(Edit::Node { id, kind });
            e.push(Edit::Parent { id, parent });
        };
        let set = |e: &mut Vec<Edit>, id: Sym, prop: Prop, value: Sym| {
            e.push(Edit::Set { id, prop, value });
        };
        // Row styles: one class per trace color, selected variants on a light background.
        let style = |e: &mut Vec<Edit>, sel: Sym, prop: StyleProp, value: u32| {
            e.push(Edit::Style {
                selector: sel,
                prop,
                value,
            });
        };
        for (class, bg) in [("wsig", 0), ("wsig-sel", 0xb4d4_ffff)] {
            let c = s(class);
            style(&mut e, c, StyleProp::Height, ROW_H as u32);
            style(&mut e, c, StyleProp::Pad, 4);
            style(&mut e, c, StyleProp::Font, 12);
            if bg != 0 {
                style(&mut e, c, StyleProp::Bg, bg);
            }
        }
        // The trace colours always have a class; a picked colour outside them gets its own.
        let mut extra: Vec<u32> = (0..waves.trace_count())
            .map(|i| waves.color(i))
            .filter(|c| !TRACE_COLORS.contains(c))
            .collect();
        extra.sort_unstable();
        extra.dedup();
        let classes = TRACE_COLORS
            .iter()
            .enumerate()
            .map(|(k, &c)| (format!("wtrace{k}"), c))
            .chain(extra.iter().map(|&c| (format!("wtrace-{c:08x}"), c)));
        for (name, color) in classes {
            for (suffix, bg) in [("", 0x2020_20ff), ("-sel", 0x5a5a_5aff)] {
                let c = s(&format!("{name}{suffix}"));
                style(&mut e, c, StyleProp::Height, ROW_H as u32);
                style(&mut e, c, StyleProp::Pad, 4);
                style(&mut e, c, StyleProp::Font, 12);
                style(&mut e, c, StyleProp::Fg, color);
                style(&mut e, c, StyleProp::Bg, bg);
                style(&mut e, c, StyleProp::HoverBg, 0x4040_40ff);
            }
        }

        // Browser: dataset title, then the visible window of the filtered signals.
        let datasets = waves.datasets();
        let title = match datasets.get(usize::from(self.dataset)) {
            Some(d) => {
                let mut name: String = d.name().chars().take(22).collect();
                if d.name().chars().count() > 22 {
                    name.push('…');
                }
                format!("{}/{} {name}", self.dataset + 1, datasets.len())
            }
            None => "no data".to_owned(),
        };
        let (dn, label) = (s("waves.dataset_name"), s(&title));
        set(&mut e, dn, Prop::Label, label);
        if let Some(d) = datasets.get(usize::from(self.dataset)) {
            let list = s("waves.signals");
            let sig_cmd = s("waves.signal");
            let (plain, sel) = (s("wsig"), s("wsig-sel"));
            let vis = self.visible_signals(waves);
            let start = (self.scroll as usize).min(vis.len());
            let end = (start + usize::from(self.signal_rows)).min(vis.len());
            for &v in &vis[start..end] {
                let var = &d.vars()[v as usize];
                let id = s(&format!("waves.sig.{v}"));
                node(&mut e, id, Kind::Item, list);
                let unit = var.kind.unit();
                let text = if unit.is_empty() {
                    var.name.clone()
                } else {
                    format!("{}  [{unit}]", var.name)
                };
                let label = s(&text);
                set(&mut e, id, Prop::Label, label);
                set(&mut e, id, Prop::Command, sig_cmd);
                let class = if self.selected_signal == Some(v) {
                    sel
                } else {
                    plain
                };
                set(&mut e, id, Prop::Style, class);
            }
        }

        // One graph node per pane, in pane order.
        let panes = s("waves.panes");
        for k in 0..waves.panes().len() {
            let id = s(&format!("waves.pane.{k}"));
            node(&mut e, id, Kind::Graph, panes);
        }

        // Trace rows (as many as fit), colored like their trace.
        let list = s("waves.traces");
        let trace_cmd = s("waves.trace");
        let shown = waves.trace_count().min(usize::from(self.trace_rows));
        for i in 0..shown {
            let id = s(&format!("waves.trace.{i}"));
            node(&mut e, id, Kind::Item, list);
            let bold = if waves.style(i) == LineStyle::Bold {
                " (bold)"
            } else {
                ""
            };
            let mag = if waves.is_magnitude(i) { "|·| " } else { "" };
            let label = s(&format!(
                "{} {mag}{}{bold}",
                waves.pane_of(i) + 1,
                waves.src(i)
            ));
            set(&mut e, id, Prop::Label, label);
            set(&mut e, id, Prop::Command, trace_cmd);
            let sel = if self.selected_trace == Some(i as u8) {
                "-sel"
            } else {
                ""
            };
            let color = waves.color(i);
            let class = match TRACE_COLORS.iter().position(|&c| c == color) {
                Some(k) => s(&format!("wtrace{k}{sel}")),
                None => s(&format!("wtrace-{color:08x}{sel}")),
            };
            set(&mut e, id, Prop::Style, class);
        }
        if waves.trace_count() > shown {
            let id = s("waves.trace.more");
            node(&mut e, id, Kind::Label, list);
            let label = s(&format!("+{} more", waves.trace_count() - shown));
            set(&mut e, id, Prop::Label, label);
        }

        // Toggle states, field values, status.
        let (on, off) = (s("1"), s("0"));
        let active = usize::from(self.active_pane);
        let pane = waves.panes().get(active);
        let log_x = if active < waves.panes().len() {
            waves.x_of(active).log
        } else {
            waves.x().log
        };
        let cur = waves.cursors();
        for (id, v) in [
            ("waves.log_x", log_x),
            ("waves.log_y", pane.is_some_and(|p| p.log)),
            ("waves.cursor_a", cur[0].is_some()),
            ("waves.cursor_b", cur[1].is_some()),
        ] {
            let id = s(id);
            set(&mut e, id, Prop::Checked, if v { on } else { off });
        }
        for (id, prop, v) in [
            ("waves.filter", Prop::Value, self.filter.as_str()),
            ("waves.expr", Prop::Value, self.expr.as_str()),
            ("waves.status_msg", Prop::Label, self.status.as_str()),
        ] {
            let (id, v) = (s(id), s(v));
            set(&mut e, id, prop, v);
        }

        // The properties dialog's working copy.
        if let Some(d) = &self.dialog {
            let mode = match d.props.mode {
                PaneMode::Analog => "analog",
                PaneMode::Digital => "digital",
                PaneMode::Histogram => "histogram",
            };
            let radix = match d.props.radix {
                Radix::Hex => "hex",
                Radix::Dec => "dec",
            };
            let title = format!("Graph properties: pane {}", d.pane + 1);
            for (id, text) in [
                ("waves.props.title", title),
                ("waves.props.mode", format!("Mode: {mode}")),
                ("waves.props.radix", format!("Radix: {radix}")),
            ] {
                let (id, v) = (s(id), s(&text));
                set(&mut e, id, Prop::Label, v);
            }
            let id = s("waves.props.unlock");
            set(
                &mut e,
                id,
                Prop::Checked,
                if d.props.locked { off } else { on },
            );
            for (k, text) in d.text.iter().enumerate() {
                let (id, v) = (s(&format!("waves.props.f.{k}")), s(text));
                set(&mut e, id, Prop::Value, v);
            }
        }
        assert!(
            e.len() <= Contribution::CAPACITY,
            "waves contribution too large"
        );
        Contribution { edits: e }
    }
}
