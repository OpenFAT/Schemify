//! The app side of the frame spine for the end-to-end tests: one host plus the tables the
//! spine owns (events, snapshots, request queues), driven one frame per `deliver`.
//!
//! Contents:
//! - `pub fn long_name() -> String`
//! - `pub fn snapshots(pin_counts: &[u32]) -> Snapshots`
//! - `pub struct World`: `new`, `load`, `frame`, `send`, `command`, `codes`

use schemify_plugin::abi::{Code, Event, Table, col};
use schemify_plugin::{Fault, PluginEvents, PluginId, Plugins, RequestQueues, Snapshots, deliver};

/// One host plus the app-side tables the spine would own.
pub struct World {
    pub plugins: Plugins,
    pub events: PluginEvents,
    pub snap: Snapshots,
    pub queues: RequestQueues,
}

/// Net 0's name, long enough that `Host::strings` overflows its first buffer guess and
/// takes the `E_SPACE` retry.
pub fn long_name() -> String {
    format!("n0{}", "x".repeat(100))
}

/// Doc 1 (`lib/top.shcomp`) and one net per pin count, net 0 named [`long_name`].
pub fn snapshots(pin_counts: &[u32]) -> Snapshots {
    assert!(!pin_counts.is_empty());
    let mut s = Snapshots::new();
    s.fill::<u32>(Table::Docs, col::docs::HANDLE, &[1]);
    s.fill::<u8>(Table::Docs, col::docs::KIND, &[1]);
    s.fill::<u8>(Table::Docs, col::docs::DIRTY, &[0]);
    s.fill_strings(Table::Docs, col::docs::PATH, &["lib/top.shcomp"]);
    s.fill_strings(Table::Docs, col::docs::CELL, &["top"]);
    let mut names: Vec<String> = (0..pin_counts.len()).map(|i| format!("n{i}")).collect();
    names[0] = long_name();
    s.fill_strings(Table::Nets, col::nets::NAME, &names);
    s.fill::<u16>(Table::Nets, col::nets::WIDTH, &vec![1; pin_counts.len()]);
    s.fill::<u8>(Table::Nets, col::nets::GLOBAL, &vec![0; pin_counts.len()]);
    s.fill::<u32>(Table::Nets, col::nets::PIN_COUNT, pin_counts);
    assert_eq!(s.check(), Ok(()));
    s
}

impl World {
    /// No plugins; nets with pin counts 5, 4, 1, 1.
    pub fn new() -> World {
        World {
            plugins: Plugins::new(),
            events: PluginEvents::new(),
            snap: snapshots(&[5, 4, 1, 1]),
            queues: RequestQueues::default(),
        }
    }

    /// Loads a module and runs the frame that delivers its `Load`.
    pub fn load(&mut self, name: &str, wasm: &[u8], grants: &[&str], fuel: u64) -> PluginId {
        let id = self.plugins.load(name, wasm, grants, fuel).unwrap();
        self.frame();
        id
    }

    /// One frame: deliver the queued events, then clear them.
    pub fn frame(&mut self) {
        deliver(
            &mut self.plugins,
            &self.events,
            &mut self.snap,
            &mut self.queues,
        );
        self.events.clear();
    }

    /// Queues `event` for `target` and runs a frame.
    pub fn send(&mut self, target: Option<PluginId>, event: Event<'_>) {
        self.events.push(target, &event).unwrap();
        self.frame();
    }

    /// Sends command `name` to `target`; returns the faults it added that the ring kept.
    pub fn command(&mut self, target: PluginId, name: &str) -> Vec<Fault> {
        let before = self.plugins.health().recorded();
        self.send(Some(target), Event::Command { name, args: "" });
        let added = usize::try_from(self.plugins.health().recorded() - before).unwrap();
        let faults = self.plugins.health().faults();
        let kept = added.min(faults.len());
        faults.iter().skip(faults.len() - kept).cloned().collect()
    }

    /// The codes of `faults`, in order.
    pub fn codes(faults: &[Fault]) -> Vec<Code> {
        faults.iter().map(|f| f.code).collect()
    }
}
