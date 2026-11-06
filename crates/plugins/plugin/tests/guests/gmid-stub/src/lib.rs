//! Example plugin proving the D20 wiring: registers the pure expression function
//! `gmid.width(gm_id, l, id)` and serves it through `schemify_call`.
//!
//! Placeholder math, not a gm/Id lookup: `W = L · I_D · (gm/I_D)² / 4`.

#![deny(unsafe_code)] // not `forbid`: the SDK's `export!` allows `unsafe(no_mangle)`

use schemify_abi::guest::{self, Host};
use schemify_abi::{Code, Event, Kind, Out, text};

guest::export!(on_event, call);

/// `fn_id` of `gmid.width`, as registered below.
const WIDTH: u32 = 0;

fn on_event(ev: Event<'_>, _host: &Host, out: &mut Out) {
    if let Event::Load = ev {
        out.contribute(Kind::Functions, "function gmid.width 0 3\n");
    }
}

fn call(fn_id: u32, args: &str, result: &mut String) -> Result<(), Code> {
    if fn_id != WIDTH {
        return Err(Code::Range);
    }
    let mut v = text::values(args);
    let (gm_id, l, id): (f64, f64, f64) = (v.need_num()?, v.need_num()?, v.need_num()?);
    v.end()?;
    let w = l * id * gm_id * gm_id / 4.0;
    if !w.is_finite() || gm_id <= 0.0 {
        return Err(Code::Range);
    }
    text::push_num(result, w);
    Ok(())
}

