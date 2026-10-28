//! The command lines (config spec §2, §7): argument words → `Startup` and `Launch`.
//!
//! Contents: no `pub` items.

use schemify_config::{GUI_USAGE, Launch, Startup};

// ---- private ----

#[test]
fn startup_flags_parse() {
    let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        Startup::parse(&args(&["netlist", "x"])),
        Ok((Startup::User, 0))
    );
    assert_eq!(
        Startup::parse(&args(&["--clean", "fmt"])),
        Ok((Startup::Clean, 1))
    );
    assert_eq!(
        Startup::parse(&args(&["-u", "t.lua", "pack", "sync"])),
        Ok((Startup::File("t.lua".into()), 2))
    );
    assert!(Startup::parse(&args(&["-u"])).is_err());
    assert!(Startup::parse(&args(&["--clean", "-u", "x"])).is_err());
    for s in [Startup::Clean, Startup::User, Startup::File("a.lua".into())] {
        let mut flags = s.flags();
        flags.push("edit".into());
        assert_eq!(Startup::parse(&flags), Ok((s.clone(), flags.len() - 1)));
    }
}

/// The editor's command line (spec §7): each word, in any order after the flags, and every
/// bad case is the usage text (after the flag's own message for a bad global flag).
#[test]
fn launch_parses_the_editor_command_line() {
    let parse = |a: &[&str]| Launch::parse(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    let only = |l: Launch| {
        Ok(Launch {
            startup: Startup::User,
            ..l
        })
    };
    assert_eq!(parse(&[]), only(Launch::default()));
    let full = Launch {
        startup: Startup::Clean,
        path: Some("top.shcomp".into()),
        no_default_ui: true,
        rpc: Some("/tmp/s".into()),
    };
    for order in [
        &[
            "--clean",
            "edit",
            "top.shcomp",
            "--no-default-ui",
            "--rpc",
            "/tmp/s",
        ][..],
        &[
            "--clean",
            "--rpc",
            "/tmp/s",
            "--no-default-ui",
            "top.shcomp",
        ],
        &[
            "--clean",
            "--no-default-ui",
            "top.shcomp",
            "--rpc",
            "/tmp/s",
        ],
    ] {
        assert_eq!(parse(order), Ok(full.clone()), "{order:?}");
    }
    assert_eq!(parse(&["edit"]), only(Launch::default()));
    assert_eq!(
        parse(&["dir"]),
        only(Launch {
            path: Some("dir".into()),
            ..Launch::default()
        })
    );
    // `edit` is only a word right after the flags; elsewhere it is a path.
    assert_eq!(
        parse(&["edit", "edit"]),
        only(Launch {
            path: Some("edit".into()),
            ..Launch::default()
        })
    );
    // The socket is the next word whatever it looks like.
    assert_eq!(
        parse(&["--rpc", "--no-default-ui"]),
        only(Launch {
            rpc: Some("--no-default-ui".into()),
            ..Launch::default()
        })
    );
    assert_eq!(
        parse(&["-u", "t.lua", "x"]),
        Ok(Launch {
            startup: Startup::File("t.lua".into()),
            path: Some("x".into()),
            ..Launch::default()
        })
    );
    for bad in [
        &["--rpc"][..],
        &["x", "--rpc"],
        &["--rpc", "a", "--rpc", "b"],
        &["a", "b"],
        &["edit", "a", "b"],
        &["--bogus"],
        &["-x", "a"],
        &["a", "--clean"],
    ] {
        assert_eq!(parse(bad), Err(GUI_USAGE.to_owned()), "{bad:?}");
    }
    assert_eq!(parse(&["-u"]), Err(format!("-u needs a file\n{GUI_USAGE}")));
    assert_eq!(
        parse(&["--clean", "-u", "x"]),
        Err(format!("give --clean or -u once\n{GUI_USAGE}"))
    );
}
