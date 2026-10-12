//! Example plugin (plugin-abi spec §0): contributes a Tools menu item and its command, and
//! logs how many nets have a single connection when the command runs.
//!
//! `hello.inspect` exercises the rest of the read path (string columns, `read_record`,
//! inverted `eval` and `fs_read`) for the host's end-to-end test.

#![deny(unsafe_code)] // not `forbid`: the SDK's `export!` allows `unsafe(no_mangle)`

use schemify_abi::guest::{self, Host};
use schemify_abi::{Event, Level, Out, Table, col};

guest::export!(on_event);

fn on_event(ev: Event<'_>, host: &Host, out: &mut Out) {
    match ev {
        // Contributions are immediate-mode: re-declare the whole set whenever it changes.
        Event::Load | Event::DocChanged { .. } => {
            let nets = host.len(Table::Nets).unwrap_or(0);
            out.contribute_ui(&format!(
                "node tools.count_nets item\n  parent menu.tools\n  label \"Count nets ({nets})\"\n  command count_nets.run\n"
            ));
            out.contribute_commands(
                "command count_nets.run \"Count nets\"\ncommand hello.inspect \"Inspect\"\n",
            );
        }
        Event::Command {
            name: "count_nets.run",
            ..
        } => {
            let nets = host.len(Table::Nets).unwrap_or(0);
            let pins = host
                .column::<u32>(Table::Nets, col::nets::PIN_COUNT, 0..nets)
                .unwrap_or_default();
            let floating = pins.iter().filter(|&&c| c == 1).count();
            out.log(&format!("{floating} single-connection nets"));
        }
        Event::Command {
            name: "hello.inspect",
            ..
        } => {
            let nets = host.len(Table::Nets).unwrap_or(0);
            let names = host.strings(Table::Nets, col::nets::NAME, 0..nets);
            let wrong_type = host.column::<u8>(Table::Nets, col::nets::PIN_COUNT, 0..nets);
            let past_end = host.column::<u32>(Table::Nets, col::nets::PIN_COUNT, 0..nets + 1);
            out.log(&format!(
                "names={names:?} wrong_type={wrong_type:?} past_end={past_end:?}"
            ));
            out.log(&format!(
                "record={:?} missing={:?}",
                host.record(1, 42),
                host.record(1, 43)
            ));
            out.log(&format!(
                "eval={:?} eval_gone={:?}",
                host.eval(1, "w*2"),
                host.eval(9, "w")
            ));
            out.log(&format!("fs_read={:?}", host.fs_read("lib/r.shprim")));
            host.log(Level::Debug, "inspected");
        }
        Event::IoDone {
            handle,
            status,
            bytes,
        } => {
            out.log(&format!(
                "io_done {handle} {status} {}",
                String::from_utf8_lossy(bytes)
            ));
        }
        _ => {}
    }
}
