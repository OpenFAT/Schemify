//! The simulated disk of the journal tests: the file and the journal sidecar exactly as
//! the app writes them, a seeded editing session that snapshots both after every
//! request, a crash-and-reopen, and idle steps run to completion.
//!
//! Contents:
//! - `pub struct Disk`: `sync`; `pub struct Snap`
//! - `pub fn snap`, `pub fn session`, `pub fn reopen`, `pub fn settle`

use super::{Mix, SEED_DOC};
use schemify_doc::{DocHandle, Edit, Mode, Op, Project, Recovery, Transaction};

#[derive(Clone, Default)]
pub struct Disk {
    pub file: Vec<u8>,
    pub journal: Vec<u8>,
}

#[derive(Clone)]
pub struct Snap {
    pub disk: Disk,
    pub state: String,
    pub history: (usize, usize),
    pub labels: Vec<String>,
    pub dirty: bool,
}

impl Disk {
    pub fn sync(&mut self, p: &mut Project, h: DocHandle) {
        if let Some(w) = p.take_journal(h) {
            if w.rewrite {
                self.journal = w.text.into_bytes();
            } else {
                self.journal.extend_from_slice(w.text.as_bytes());
            }
        }
    }
}

pub fn snap(p: &Project, h: DocHandle, disk: &Disk) -> Snap {
    Snap {
        disk: disk.clone(),
        state: super::canon(p, h),
        history: p.history_len(h),
        labels: p.undo_labels(h).iter().map(|s| s.to_string()).collect(),
        dirty: p.is_dirty(h),
    }
}

/// Runs a seeded session; returns the snapshot after every request (index 0 = opened).
pub fn session(seed: u64, steps: usize, saves: bool) -> Vec<Snap> {
    let mut rng = Mix::new(seed);
    let (mut p, h) = super::open(SEED_DOC, "cell.shcomp");
    let mut disk = Disk {
        file: SEED_DOC.as_bytes().to_vec(),
        journal: Vec::new(),
    };
    let mut snaps = vec![snap(&p, h, &disk)];
    for step in 0..steps {
        // The editor's idle frames hash the disk state ahead of the commits (in part).
        if rng.below(3) == 0 {
            p.idle_step(Some(h), 1 + rng.below(64) as usize);
        }
        match rng.below(10) {
            0..=5 => {
                let ops: Vec<Op> = (0..1 + rng.below(2))
                    .map(|_| super::gen_op(&mut rng, super::doc(&p, h)))
                    .collect();
                let edit = Edit::Tx(Transaction {
                    label: format!("step {step}"),
                    mode: Mode::Commit,
                    ops,
                });
                super::run(&mut p, h, edit);
            }
            6 | 7 => {
                super::run(&mut p, h, Edit::Undo);
            }
            8 => {
                super::run(&mut p, h, Edit::Redo);
            }
            _ if saves => {
                if let Some(req) = p.save(h).expect("saving an open document builds a request") {
                    disk.file = req.bytes.clone().into_bytes(); // atomic write, then confirm
                    p.saved(h, req.bytes)
                        .expect("the save request was just issued for this document");
                }
            }
            _ => {}
        }
        disk.sync(&mut p, h);
        snaps.push(snap(&p, h, &disk));
    }
    snaps
}

/// Opens a crashed disk state and recovers whatever the journal offers.
pub fn reopen(disk: &Disk) -> (Project, DocHandle, Recovery) {
    let mut p =
        Project::new(None, None, 99).expect("a project without manifest or lock always opens");
    let journal = (!disk.journal.is_empty()).then_some(disk.journal.as_slice());
    let opened = p
        .open("", "cell.shcomp", &disk.file, journal)
        .expect("the session's own file reopens");
    let h = opened.handle;
    if matches!(
        opened.recovery,
        Recovery::Offer { .. } | Recovery::Diverged { .. }
    ) {
        p.recover(h).expect("the session's own journal recovers");
    }
    (p, h, opened.recovery)
}

/// Hashes, compacts and encodes until `Project::idle_step` has nothing left (the editor's
/// idle frames); returns the number of steps.
pub fn settle(p: &mut Project, h: DocHandle, budget: usize) -> usize {
    let mut steps = 0;
    while p.idle_pending(Some(h)) {
        p.idle_step(Some(h), budget);
        steps += 1;
        assert!(steps < 1 << 20, "idle steps end");
    }
    steps
}
