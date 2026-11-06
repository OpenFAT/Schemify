//! A Schemify plugin in Rust on the `schemify-abi` guest SDK: Tools → Count nets logs the
//! nets with a single connection. `export!` writes the module's exports; `Host` wraps the
//! host imports; `Out` writes the output records.

use schemify_abi::guest::{self, Host};
use schemify_abi::{Event, Out, Table, col};

guest::export!(on_event);

fn on_event(ev: Event<'_>, host: &Host, out: &mut Out) {
    match ev {
        // Contributions are immediate-mode: send the whole set every time.
        Event::Load | Event::DocChanged { .. } => {
            out.contribute_ui(
                "node tools.hello item\n  parent menu.tools\n  label \"Count nets\"\n  command hello.count\n",
            );
            out.contribute_commands("command hello.count \"Count nets\"\n");
        }
        Event::Command {
            name: "hello.count",
            ..
        } => {
            let n = host.len(Table::Nets).unwrap_or(0);
            let pins = host.column::<u32>(Table::Nets, col::nets::PIN_COUNT, 0..n);
            let names = host.strings(Table::Nets, col::nets::NAME, 0..n);
            let single: Vec<String> = match (pins, names) {
                (Ok(pins), Ok(names)) => (pins.into_iter().zip(names))
                    .filter_map(|(p, name)| (p == 1).then_some(name))
                    .collect(),
                _ => Vec::new(),
            };
            let list = if single.is_empty() {
                String::new()
            } else {
                format!(": {}", single.join(", "))
            };
            out.log(&format!("{} single-connection nets{list}", single.len()));
        }
        _ => {}
    }
}
