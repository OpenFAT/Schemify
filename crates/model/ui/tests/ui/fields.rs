//! Text fields: focus, typing with an `input` command per change, Enter submitting.
//!
//! Contents: no `pub` items.

use schemify_ui::InputEvents;

// ---- private ----

/// A field with an `input` command sends its text as typed and its `command` on Enter.
#[test]
fn a_field_sends_input_as_typed_and_submits_on_enter() {
    let src = "node t.box text_input\n  parent status\n  value ab\n  input t.typed\n  command t.done\n";
    let mut f = crate::fixture(&[src]);
    f.click("t.box");
    assert_eq!(f.state.focus(), f.sym("t.box"), "a click focuses the field");
    let mut ev = InputEvents::default();
    ev.push_text("c");
    let out = f.route(&ev);
    assert_eq!(f.commands(&out), ["t.typed"]);
    assert_eq!(out.commands.arg(0), "abc");
    let out = crate::key(&mut f, "Ctrl+S");
    assert!(out.commands.is_empty(), "bindings wait while a field has the keyboard");
    let out = crate::key(&mut f, "Enter");
    assert_eq!(f.commands(&out), ["t.done"]);
    assert_eq!(out.commands.arg(0), "abc");
    assert!(f.state.focus().is_none(), "Enter leaves the field");
}

/// A host focuses a field the next time it shows ([`schemify_ui::UiState::focus_text`]).
#[test]
fn focus_text_focuses_on_the_next_frame() {
    let src = "node t.box text_input\n  parent status\n  value ab\n  command t.done\n";
    let mut f = crate::fixture(&[src]);
    f.state.focus_text(f.sym("t.box"));
    f.route(&InputEvents::default());
    f.route(&InputEvents::default());
    assert_eq!(f.state.focus(), f.sym("t.box"));
}
