//! Commands of the open dialog: properties, chooser, enter text, edit file, recovery.
//!
//! Contents:
//! - `Editor::run_dialog`

use schemify_doc::EditRequest;

use crate::commands::Cmd;

use super::Editor;
use super::dialog::Dialog;
use super::tool::Tool;

impl Editor {
    /// Commands of the open dialog: properties, chooser, enter text, edit file, recovery. Returns whether anything visible changed.
    pub(super) fn run_dialog(
        &mut self,
        cmd: Cmd,
        arg: &str,
        node: &str,
        reqs: &mut Vec<EditRequest>,
    ) -> bool {
        let Some((h, _)) = self.active() else {
            return false;
        };
        let ptr = self.pointer_world_snapped();
        match cmd {
            Cmd::Props | Cmd::PropsEditor | Cmd::PropsView => self.open_props(h),
            Cmd::PropsField => self.props_field(node, arg),
            Cmd::PropsOk => self.props_ok(h, reqs),
            Cmd::DialogClose => self.close_dialog(),
            Cmd::ChooseFilter => self.choose_filter(arg),
            Cmd::ChoosePick => self.choose_pick(node, h),
            Cmd::EnterTextOk => self.enter_text_ok(arg, ptr, reqs),
            Cmd::EditFile => {
                let text = self
                    .session
                    .project
                    .doc(h)
                    .map(schemify_doc::Document::canonical)
                    .unwrap_or_default();
                self.tool = Tool::Idle;
                self.dialog = Dialog::File(text);
                self.show_dialog("dialog.edit_file");
                self.focus_next = Some("dialog.edit_file.text".into());
            }
            Cmd::EditFileOk => {
                let Dialog::File(typed) = std::mem::take(&mut self.dialog) else {
                    return false;
                };
                self.close_dialog();
                let text = if arg.is_empty() {
                    typed
                } else {
                    arg.to_owned()
                };
                self.apply_file_text(reqs, h, &text);
            }
            Cmd::EditHeader => self.open_header(h),
            Cmd::ContextMenu => self.context_menu(self.pointer),
            Cmd::Recover | Cmd::DiscardRecovery => {
                let Dialog::Recover(d, _) = std::mem::take(&mut self.dialog) else {
                    return false;
                };
                self.close_dialog();
                self.session.decide_recovery(d, cmd == Cmd::Recover);
                self.invalidate_cells();
            }
            _ => unreachable!("{cmd:?}: not a dialog command"),
        }
        true
    }
}
