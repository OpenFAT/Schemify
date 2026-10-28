//! Tabs and the hierarchy: opening and switching tabs, the status line, each level's
//! camera, selection and wire ends, descending into an instance and back.
//!
//! Contents:
//! - `pub const STATUS_MAX`
//! - `pub struct TabView`
//! - `Session::open_tab`
//! - `Session::open_text_tab`
//! - `Session::status`
//! - `Session::say`
//! - `Session::take_status`
//! - `Session::text_view`
//! - `Session::tabs`
//! - `Session::active`
//! - `Session::switch_tab`
//! - `Session::close_active_tab`
//! - `Session::tab_view_mut`
//! - `Session::text_view_mut`
//! - `Session::set_camera`
//! - `Session::select`
//! - `Session::set_ends`
//! - `Session::show`
//! - `Session::level`
//! - `Session::hier_path`
//! - `Session::push_level`
//! - `Session::pop_level`
//! - `pub(super) fn cut`

use std::mem;

use schemify_canvas::Camera;
use schemify_doc::{DocHandle, TableId};
use schemify_format::FileKind;

use crate::geometry::Pt;
use crate::scene::ObjKey;

use super::{DEPTH_MAX, Level, Session, TABS_MAX, Tab, TextView, View};

/// The longest status line in bytes; longer messages are cut.
pub const STATUS_MAX: usize = 1 << 14;

/// The active tab's view toggles, lent by [`Session::tab_view_mut`].
pub struct TabView<'tab> {
    /// The docs page is shown beside (or instead of) the canvas.
    pub docs: &'tab mut bool,
    /// The docs page shows its source text.
    pub docs_source: &'tab mut bool,
    /// The cell settings panel is shown.
    pub cell: &'tab mut bool,
}

impl Session {
    /// Opens a new tab showing `doc` (or switches to the tab already showing it at top).
    ///
    /// # Panics
    ///
    /// Never: the asserts check the tab count against [`TABS_MAX`].
    pub fn open_tab(&mut self, doc: DocHandle, view: View) {
        assert!(self.tabs.len() <= TABS_MAX);
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.stack[0].doc == doc && t.stack[0].view == view)
        {
            self.active = i;
            return;
        }
        if self.tabs.len() >= TABS_MAX {
            self.status = format!("at most {TABS_MAX} tabs");
            return;
        }
        let docs = self
            .project
            .doc(doc)
            .is_some_and(|d| !d.doc_blocks().kind.is_empty());
        self.tabs.push(Tab {
            stack: vec![Level {
                doc,
                view,
                inst: String::new(),
                cam: None,
                sel: Vec::new(),
                ends: Vec::new(),
            }],
            docs,
            docs_source: false,
            cell: false,
            text: None,
        });
        self.active = self.tabs.len() - 1;
        assert!(self.tabs.len() <= TABS_MAX);
    }

    /// Shows `view` in a read-only text tab made from the active level (a tab already
    /// showing the same file is replaced and activated).
    ///
    /// # Panics
    ///
    /// Never: the asserts check the tab count against [`TABS_MAX`].
    pub fn open_text_tab(&mut self, view: TextView) {
        assert!(self.tabs.len() <= TABS_MAX);
        let Some(level) = self.level().cloned() else {
            return;
        };
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.text.as_ref().is_some_and(|v| v.path == view.path))
        {
            self.tabs[i].text = Some(view);
            self.active = i;
            return;
        }
        if self.tabs.len() >= TABS_MAX {
            self.status = format!("at most {TABS_MAX} tabs");
            return;
        }
        self.tabs.push(Tab {
            stack: vec![level],
            docs: false,
            docs_source: false,
            cell: false,
            text: Some(view),
        });
        self.active = self.tabs.len() - 1;
        assert!(self.tabs.len() <= TABS_MAX);
    }

    /// The status line: the message the last step left (last wins).
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Sets the status line; the newest message wins. A message past [`STATUS_MAX`] bytes is
    /// cut at a character boundary (a plugin's error text is untrusted in length).
    ///
    /// # Panics
    ///
    /// Never: the message is cut to [`STATUS_MAX`] bytes.
    pub fn say(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        cut(&mut self.status);
        assert!(self.status.len() <= STATUS_MAX);
    }

    /// Takes the status line and leaves it empty: a step that runs a command on a clean
    /// line and puts the old message back when the command says nothing.
    pub fn take_status(&mut self) -> String {
        mem::take(&mut self.status)
    }

    /// The text view of the active tab, if it is one.
    #[must_use]
    pub fn text_view(&self) -> Option<&TextView> {
        self.tabs.get(self.active).and_then(|t| t.text.as_ref())
    }

    /// The open tabs, in tab-bar order.
    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        debug_assert!(self.tabs.len() <= TABS_MAX);
        &self.tabs
    }

    /// The active tab's index (`< tabs().len()` once a tab is open).
    ///
    /// # Panics
    ///
    /// Never: the assert checks that the index names an open tab when one is open.
    #[must_use]
    pub fn active(&self) -> usize {
        assert!(self.tabs.is_empty() || self.active < self.tabs.len());
        self.active
    }

    /// Makes tab `i` the active one.
    ///
    /// # Panics
    ///
    /// If `i` is not an open tab.
    pub fn switch_tab(&mut self, i: usize) {
        assert!(i < self.tabs.len(), "tab {i} of {}", self.tabs.len());
        self.active = i;
    }

    /// Closes the active tab, opening a new schematic when it was the last; the next tab
    /// to the left becomes active.
    ///
    /// # Errors
    ///
    /// The new schematic cannot be created (the tab is closed regardless).
    ///
    /// # Panics
    ///
    /// If no tab is open.
    pub fn close_active_tab(&mut self) -> Result<(), String> {
        assert!(self.active < self.tabs.len(), "an active tab to close");
        self.tabs.remove(self.active);
        let made = match self.tabs.is_empty() {
            true => self
                .create(FileKind::Shcomp)
                .map(|d| self.open_tab(d, View::Schematic)),
            false => Ok(()),
        };
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        assert!(
            self.tabs.is_empty() || self.active < self.tabs.len(),
            "the active index names an open tab"
        );
        assert!(
            made.is_err() || !self.tabs.is_empty(),
            "the last tab is replaced"
        );
        made
    }

    /// The active tab, mutably, for its view toggles (docs page, its source, the cell panel).
    pub fn tab_view_mut(&mut self) -> Option<TabView<'_>> {
        self.tabs.get_mut(self.active).map(|t| TabView {
            docs: &mut t.docs,
            docs_source: &mut t.docs_source,
            cell: &mut t.cell,
        })
    }

    /// The active tab's text view, mutably (its find highlight), if it is one.
    pub fn text_view_mut(&mut self) -> Option<&mut TextView> {
        self.tabs.get_mut(self.active).and_then(|t| t.text.as_mut())
    }

    /// Sets the active level's camera.
    ///
    /// # Panics
    ///
    /// If `cam.scale` is not positive or its center is not finite.
    pub fn set_camera(&mut self, cam: Camera) {
        assert!(cam.scale > 0.0, "a positive zoom");
        assert!(
            cam.cx.is_finite() && cam.cy.is_finite(),
            "a finite camera center"
        );
        if let Some(l) = self.level_mut() {
            l.cam = Some(cam);
        }
    }

    /// Replaces the active level's selection with `sel` (sorted, distinct); returns whether
    /// it changed.
    pub fn select(&mut self, mut sel: Vec<ObjKey>) -> bool {
        sel.sort_unstable();
        sel.dedup();
        let Some(l) = self.level_mut() else {
            return false;
        };
        let changed = l.sel != sel;
        if changed {
            l.sel = sel;
        }
        changed
    }

    /// Replaces the active level's partial wire ends (sorted, distinct; only wire rows).
    pub fn set_ends(&mut self, mut ends: Vec<(ObjKey, Pt)>) {
        ends.retain(|e| e.0.table == TableId::Wires);
        ends.sort_unstable();
        ends.dedup();
        if let Some(l) = self.level_mut() {
            l.ends = ends;
        }
    }

    /// Shows `doc` in `view` at the active level: the selection and the partial ends are
    /// dropped (they name another document's rows), and with `refit` the camera too (the
    /// first build fits it). Returns whether a selection was dropped.
    ///
    pub fn show(&mut self, doc: DocHandle, view: View, refit: bool) -> bool {
        let Some(l) = self.level_mut() else {
            return false;
        };
        l.doc = doc;
        l.view = view;
        if refit {
            l.cam = None;
        }
        let had = !l.sel.is_empty();
        l.sel.clear();
        l.ends.clear();
        had
    }

    /// The level shown in the active tab.
    #[must_use]
    pub fn level(&self) -> Option<&Level> {
        let tab = self.tabs.get(self.active);
        let level = tab.and_then(|t| t.stack.last());
        debug_assert_eq!(
            tab.is_some(),
            level.is_some(),
            "a tab's level stack is never empty"
        );
        level
    }

    /// The level shown in the active tab, mutably: only `Session`'s own transforms write a
    /// level (selection, camera, partial ends, the shown document).
    fn level_mut(&mut self) -> Option<&mut Level> {
        self.tabs
            .get_mut(self.active)
            .and_then(|t| t.stack.last_mut())
    }

    /// Hierarchy path of the active tab, xschem style (`.x1.x2.`).
    #[must_use]
    pub fn hier_path(&self) -> String {
        let mut s = String::from(".");
        if let Some(t) = self.tabs.get(self.active) {
            for l in &t.stack[1..] {
                s.push_str(&l.inst);
                s.push('.');
            }
        }
        s
    }

    /// Pushes a level onto the active tab (descend).
    ///
    /// # Errors
    ///
    /// The tab is already [`DEPTH_MAX`] deep.
    ///
    /// # Panics
    ///
    /// Never: the depth is checked against [`DEPTH_MAX`] first.
    pub fn push_level(&mut self, doc: DocHandle, view: View, inst: &str) -> Result<(), String> {
        let tab = self.tabs.get_mut(self.active).ok_or("no tab")?;
        if tab.stack.len() >= DEPTH_MAX {
            return Err(format!("hierarchy deeper than {DEPTH_MAX}"));
        }
        tab.stack.push(Level {
            doc,
            view,
            inst: inst.to_owned(),
            cam: None,
            sel: Vec::new(),
            ends: Vec::new(),
        });
        assert!(tab.stack.len() <= DEPTH_MAX);
        Ok(())
    }

    /// Pops the active tab's level (return to parent); false at the top.
    ///
    /// # Panics
    ///
    /// Never: only a level above the top one is popped.
    pub fn pop_level(&mut self) -> bool {
        match self.tabs.get_mut(self.active) {
            Some(t) if t.stack.len() > 1 => {
                t.stack.pop();
                assert!(!t.stack.is_empty(), "the top level stays");
                true
            }
            _ => false,
        }
    }
}

/// Cuts `s` to at most [`STATUS_MAX`] bytes at a character boundary.
pub(super) fn cut(s: &mut String) {
    if s.len() > STATUS_MAX {
        let mut at = STATUS_MAX;
        while !s.is_char_boundary(at) {
            at -= 1;
        }
        s.truncate(at);
        assert!(at + 4 > STATUS_MAX, "a char is at most four bytes");
    }
    assert!(s.len() <= STATUS_MAX);
}
