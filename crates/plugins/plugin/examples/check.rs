//! Loads a plugin module into the real host and shows what it does, for authors in any
//! language (docs/plugins/languages.md):
//!
//! ```sh
//! cargo run -p schemify-plugin --example check -- my_plugin.wasm [command…]
//! ```
//!
//! Delivers `Load`, then each named command, against nets a, b, c, d with pin counts 5, 4, 1, 1;
//! prints the composed contributions, the logs and the health faults. Exits 1 when the
//! module fails to load or anything faults.

use schemify_plugin::abi::{Event, FUEL_DEFAULT, Table, col};
use schemify_plugin::{PluginEvents, Plugins, RequestQueues, Snapshots, deliver};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: check <module.wasm> [command…]");
        std::process::exit(2);
    };
    let wasm = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut snap = Snapshots::new();
    snap.fill_strings(Table::Nets, col::nets::NAME, &["a", "b", "c", "d"]);
    snap.fill::<u16>(Table::Nets, col::nets::WIDTH, &[1; 4]);
    snap.fill::<u8>(Table::Nets, col::nets::GLOBAL, &[0; 4]);
    snap.fill::<u32>(Table::Nets, col::nets::PIN_COUNT, &[5, 4, 1, 1]);
    let mut plugins = Plugins::new();
    let mut events = PluginEvents::new();
    let mut queues = RequestQueues::default();
    let id = match plugins.load("check", &wasm, &[], FUEL_DEFAULT) {
        Ok(id) => id,
        Err(code) => {
            println!("load failed: {code:?}");
            for f in plugins.health().faults() {
                println!("fault {:?}: {}", f.code, f.detail);
            }
            std::process::exit(1);
        }
    };
    println!("{path}: {} bytes, loaded", wasm.len());
    deliver(&mut plugins, &events, &mut snap, &mut queues);
    for r in &plugins.composed().records {
        println!(
            "contribute {}: {}",
            r.kind.name(),
            r.text.replace('\n', "\n    ")
        );
    }
    for name in args {
        events.clear();
        if events
            .push(
                Some(id),
                &Event::Command {
                    name: &name,
                    args: "",
                },
            )
            .is_err()
        {
            println!("command {name}: not a bare word");
            std::process::exit(2);
        }
        deliver(&mut plugins, &events, &mut snap, &mut queues);
    }
    for l in &queues.logs {
        println!("log {}: {}", l.level.name(), l.text);
    }
    let faults = plugins.health().faults();
    for f in faults {
        println!("fault {:?}: {}", f.code, f.detail);
    }
    std::process::exit(i32::from(!faults.is_empty()));
}
