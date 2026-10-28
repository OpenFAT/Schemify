//! Health report (D8): faults attributed to plugins in, a bounded ring of recent faults
//! plus the current composition's overrides out. Cites plugin-abi §2 (faults, quarantine)
//! and §8 (rejections).
//!
//! Contents:
//! - `pub struct Fault`
//! - `pub struct Health`: `faults`, `dropped`, `recorded`, `overrides`;
//!   `pub(crate) fn fault(&mut self, plugin, code, detail)`

use std::collections::VecDeque;

use schemify_abi::Code;

use crate::{FAULTS_MAX, Override, PluginId};

/// One declared error attributed to a plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    /// The plugin (for a failed load: the id it would have had).
    pub plugin: PluginId,
    /// What went wrong.
    pub code: Code,
    /// Human-readable context.
    pub detail: String,
}

/// Health report (D8): recent faults (a ring of at most [`FAULTS_MAX`]) and the overrides
/// of the current composition. Written only by [`crate::Plugins`].
#[derive(Clone, Debug, Default)]
pub struct Health {
    faults: VecDeque<Fault>,
    dropped: u32,
    recorded: u64,
    pub(crate) overrides: Vec<Override>,
}

impl Health {
    /// Returns the most recent faults, oldest first, at most [`FAULTS_MAX`].
    pub fn faults(&self) -> &VecDeque<Fault> {
        &self.faults
    }

    /// Returns how many faults were dropped to keep the ring bounded (saturating).
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Returns how many faults were ever recorded: a reader that remembers it finds the
    /// faults recorded since as the last `recorded() - seen` of [`Health::faults`] (fewer if
    /// the ring dropped some meanwhile).
    pub fn recorded(&self) -> u64 {
        self.recorded
    }

    /// Returns every record of the current composition that a later plugin overrode.
    pub fn overrides(&self) -> &[Override] {
        &self.overrides
    }

    /// Records one fault, dropping the oldest at [`FAULTS_MAX`].
    pub(crate) fn fault(&mut self, plugin: PluginId, code: Code, detail: String) {
        debug_assert!(self.faults.len() <= FAULTS_MAX);
        if self.faults.len() == FAULTS_MAX {
            self.faults.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.faults.push_back(Fault {
            plugin,
            code,
            detail,
        });
        self.recorded += 1;
        debug_assert!(self.faults.len() <= FAULTS_MAX);
        debug_assert!(self.recorded >= self.faults.len() as u64);
    }
}
