//! Guest glue (`wasm32-unknown-unknown` only). It contributes nothing yet: the import and export commands
//! arrive with the Virtuoso and xschem export work (D24 as amended 2026-10-12).

use schemify_abi::guest::{self, Host};
use schemify_abi::{Event, Out};

guest::export!(on_event);

// ponytail: a no-op guest so the plugin builds and links as wasm; commands
// (`interop.xschem_import`, …) go here as they land.
fn on_event(_ev: Event<'_>, _host: &Host, _out: &mut Out) {}
