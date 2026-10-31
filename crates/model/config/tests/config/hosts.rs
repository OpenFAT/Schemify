//! `sim` rows on other machines (config spec §3, run spec §5 remote jobs): Lua source →
//! [`Table::hosts`](schemify_config::Table::hosts) rows, and the ssh destination grammar's
//! negative space.
//!
//! Contents: no `pub` items.

use schemify_config::{HOST_SIMS_MAX, SimKind, valid_host};

use crate::{errors, run};

// ---- private ----

/// `schemify.sim(name, { host = … })` (spec §3): a row of that simulator on that ssh
/// destination, kept beside the local row and keyed by (simulator, host), `path` optional;
/// a destination outside the grammar (an option, a port, an IPv6 literal, a space), a
/// compiler with a host, or a control character in a remote word is one located error that
/// leaves the rows as they were.
#[test]
fn sim_rows_name_the_hosts_jobs_may_run_on() {
    let src = "schemify.sim('espice', { path = '/home/me/espice' })\n\
               schemify.sim('espice', { host = 'pdk@server.example.edu', path = '/opt/espice/bin/espice', env = { ZIG = '/opt/zig' } })\n\
               schemify.sim('spectre', { host = 'server.example.edu' })\n\
               schemify.sim('espice', { host = 'pdk@server.example.edu', args = { '-b', '{netlist}' } })\n";
    let (cfg, out) = run(src);
    assert!(out.is_empty(), "{out:?}");
    let t = cfg.table();
    assert_eq!(
        t.sim(SimKind::Espice).unwrap().path,
        "/home/me/espice",
        "the local row stays"
    );
    assert_eq!(
        t.hosts.len(),
        2,
        "a second row of one (simulator, host) replaces the first"
    );
    let espice = t.sim_on(SimKind::Espice, "pdk@server.example.edu").unwrap();
    assert_eq!(
        espice.path, "",
        "no `path`: the default program on the host"
    );
    assert_eq!(espice.args, ["-b", "{netlist}"]);
    assert!(espice.env.is_empty());
    let spectre = t.sim_on(SimKind::Spectre, "server.example.edu").unwrap();
    assert_eq!(spectre.path, "");
    assert!(
        t.sim_on(SimKind::Spectre, "pdk@server.example.edu")
            .is_none(),
        "exact destination"
    );
    assert!(t.sim_on(SimKind::Ngspice, "server.example.edu").is_none());

    for ok in [
        "h",
        "server.example.edu",
        "pdk@h-1.x",
        "_ci@10.0.0.7",
        "Alias2",
    ] {
        assert!(valid_host(ok), "{ok}");
    }
    let long = "a".repeat(254);
    for bad in [
        "",
        "-oProxyCommand=x",
        "h -p 22",
        "h:22",
        "[::1]",
        "a@b@c",
        "Pdk@h",
        "@h",
        "u@",
        ".h",
        "h;rm",
        "h`x`",
        long.as_str(),
    ] {
        assert!(!valid_host(bad), "{bad}");
        let (cfg, out) = run(&format!("schemify.sim('espice', {{ host = '{bad}' }})"));
        let e = errors(&out);
        assert_eq!(e.len(), 1, "{bad}: {out:?}");
        assert!(
            e[0].starts_with("init.lua:1: ") && e[0].contains("ssh destination"),
            "{}",
            e[0]
        );
        assert!(cfg.table().hosts.is_empty());
    }
    for (bad, why) in [
        (
            "schemify.sim('openvaf', { host = 'h', path = 'openvaf' })",
            "a compiler runs here",
        ),
        (
            "schemify.sim('espice', { host = 'h', args = { 'a\\nb' } })",
            "no control characters",
        ),
        (
            "schemify.sim('espice', { host = 'h', env = { ZIG = 'x\\ty' } })",
            "no control characters",
        ),
        (
            "schemify.sim('espice', { host = { 'h' } })",
            "ssh destination",
        ),
    ] {
        let (cfg, out) = run(bad);
        let e = errors(&out);
        assert_eq!(e.len(), 1, "{bad}: {out:?}");
        assert!(e[0].contains(why), "{bad}: {}", e[0]);
        assert!(cfg.table().hosts.is_empty() && cfg.table().sim(SimKind::Openvaf).is_none());
    }
    // A local row still needs its program.
    let (_, out) = run("schemify.sim('espice', {})");
    assert!(errors(&out)[0].contains("`path` is required"));
    // Capacity: the 65th host is refused, the first 64 stay.
    let many: String = (0..=HOST_SIMS_MAX)
        .map(|i| format!("schemify.sim('espice', {{ host = 'h{i}' }})\n"))
        .collect();
    let (cfg, out) = run(&many);
    assert_eq!(cfg.table().hosts.len(), HOST_SIMS_MAX);
    assert!(errors(&out)[0].contains(&format!("more than {HOST_SIMS_MAX}")));
}
