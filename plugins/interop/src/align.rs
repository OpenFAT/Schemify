//! Pin-pattern alignment (spec `interop.md` §6.4): where to put a replacement master so its
//! pins land on the pins of the master it replaces.
//!
//! Two symbols are interchangeable when one's pin pattern is the other's moved: a Schemify
//! `nmos4`, analogLib's `nmos4` and a PDK's nfet with the same drain/gate/source/bulk layout
//! put their origins in different places but their pins in the same pattern. [`fit`] finds
//! the origin for the replacement in the instance's own orientation, so a re-mastered
//! device keeps every wire and needs no stub; pins that still miss get stubs from the
//! caller ([`crate::remap`]). It knows nothing about libraries or device kinds: the pairing
//! of pins comes from the caller (the §6.5 table, `interop.map` pin rows).

use schemify_format::Orient;

use crate::design::place;

/// Where a replacement master goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fit {
    /// The replacement's origin, milli-units.
    pub origin: [i32; 2],
    /// Per pair: its pin lands on the old pin at `origin`.
    pub hit: Vec<bool>,
}

impl Fit {
    /// Every paired pin lands.
    pub fn full(&self) -> bool {
        self.hit.iter().all(|h| *h)
    }
}

/// Pairs at most this many pins (a device symbol has a handful; a bound for the
/// quadratic search).
pub const PAIRS_MAX: usize = 1024;

/// The origin, in orientation `o`, that puts the most replacement pins on the old ones.
/// `old[k]` is the old pin's drawn point (milli-units, the schematic's frame) and `new[k]`
/// the paired replacement pin in its own symbol frame, `None` when the replacement has no
/// such terminal. A fit lands at least two pins, or every pin when there is only one:
/// moving a device for one pin out of several gains nothing. `None` otherwise, or with no
/// pair to place by.
///
/// # Panics
///
/// When `old` and `new` differ in length or exceed [`PAIRS_MAX`].
pub fn fit(old: &[[i32; 2]], new: &[Option<[i32; 2]>], o: Orient) -> Option<Fit> {
    assert_eq!(old.len(), new.len(), "pins pair one to one");
    assert!(old.len() <= PAIRS_MAX, "too many pins to pair");
    let turned: Vec<Option<[i32; 2]>> = new
        .iter()
        .map(|p| p.and_then(|p| place(p, o, [0, 0])))
        .collect();
    let candidates = turned.iter().filter(|p| p.is_some()).count();
    let mut best: Option<Fit> = None;
    let mut best_n = 0;
    for (k, t) in turned.iter().enumerate() {
        let Some(t) = t else { continue };
        let origin = [old[k][0] - t[0], old[k][1] - t[1]];
        let hit: Vec<bool> = turned
            .iter()
            .zip(old)
            .map(|(p, q)| p.is_some_and(|p| [origin[0] + p[0], origin[1] + p[1]] == *q))
            .collect();
        let n = hit.iter().filter(|h| **h).count();
        if n > best_n {
            best_n = n;
            best = Some(Fit { origin, hit });
        }
    }
    let enough = best_n >= 2 || (best_n == 1 && candidates == 1);
    let out = best.filter(|_| enough);
    debug_assert!(out.as_ref().is_none_or(|f| f.hit.len() == old.len()));
    out
}

/// [`fit`] for one placed instance: the origin of its replacement master. The instance sits
/// at `at` in orientation `o`; `old_at` holds its old master's symbol-local pins, `pairs`
/// `(old pin index, new pin name)` the connections to carry, `new_pins` the replacement's
/// `(name, symbol-local point)`. A pair whose name the replacement lacks places nothing.
/// Export (onto analogLib or a map row) and import (back onto a core cell) both place by it.
///
/// # Panics
///
/// When a pair indexes past `old_at`, or as [`fit`].
pub fn fit_instance(
    old_at: &[[i32; 2]],
    pairs: &[(usize, String)],
    new_pins: &[(String, [i32; 2])],
    o: Orient,
    at: [i32; 2],
) -> Option<[i32; 2]> {
    assert!(
        pairs.iter().all(|(p, _)| *p < old_at.len()),
        "a pair names a pin the old master lacks"
    );
    let old: Vec<[i32; 2]> = pairs
        .iter()
        .map(|(p, _)| place(old_at[*p], o, at).unwrap_or(at))
        .collect();
    let new: Vec<Option<[i32; 2]>> = pairs
        .iter()
        .map(|(_, nm)| new_pins.iter().find(|(x, _)| x == nm).map(|(_, p)| *p))
        .collect();
    fit(&old, &new, o).map(|f| f.origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const ORIENTS: [Orient; 8] = [
        Orient::R0,
        Orient::R90,
        Orient::R180,
        Orient::R270,
        Orient::MR0,
        Orient::MR90,
        Orient::MR180,
        Orient::MR270,
    ];

    #[test]
    fn a_translated_pattern_fits_and_a_different_one_does_not() {
        // Core nmos4 at (100, 50) against analogLib's nmos4 (its pins 20 to the right).
        let ours = [
            [20_000, -30_000],
            [-20_000, 0],
            [20_000, 30_000],
            [20_000, 0],
        ];
        let old: Vec<[i32; 2]> = ours
            .iter()
            .map(|p| [p[0] + 100_000, p[1] + 50_000])
            .collect();
        let alib = [[40_000, -30_000], [0, 0], [40_000, 30_000], [40_000, 0]].map(Some);
        let f = fit(&old, &alib, Orient::R0).expect("congruent");
        assert!(f.full());
        assert_eq!(f.origin, [80_000, 50_000]);
        // tline: no translation lands two pins.
        let old = [
            [-80_000, -10_000],
            [-80_000, 10_000],
            [80_000, -10_000],
            [80_000, 10_000],
        ];
        let alib = [
            [-40_000, 0],
            [-40_000, 60_000],
            [40_000, 0],
            [40_000, 60_000],
        ]
        .map(Some);
        assert_eq!(fit(&old, &alib, Orient::R0), None);
    }

    #[test]
    fn a_partial_fit_names_the_pins_that_miss() {
        // vcvs: the output pins line up after a move, the control pins do not.
        let old = [
            [0, -30_000],
            [0, 30_000],
            [-40_000, -20_000],
            [-40_000, 20_000],
        ];
        let alib = [[0, 0], [0, 60_000], [-20_000, 20_000], [-20_000, 40_000]].map(Some);
        let f = fit(&old, &alib, Orient::R0).expect("two pins land");
        assert_eq!(f.origin, [0, -30_000]);
        assert_eq!(f.hit, [true, true, false, false]);
        assert!(!f.full());
    }

    #[test]
    #[should_panic(expected = "a pair names a pin the old master lacks")]
    fn instance_fits_check_their_pairs() {
        fit_instance(&[[0, 0]], &[(1, "a".into())], &[], Orient::R0, [0, 0]);
    }

    #[test]
    fn a_missing_terminal_and_no_pairs() {
        let old = [[0, 0], [0, 60_000]];
        assert_eq!(fit(&old, &[None, None], Orient::R0), None);
        let f = fit(&old, &[Some([5_000, 5_000]), None], Orient::R0).expect("one pin");
        assert_eq!(f.hit, [true, false]);
        assert_eq!(fit(&[], &[], Orient::R0), None);
    }

    proptest! {
        /// `fit_instance`'s origin is a certificate: placed there in the instance's
        /// orientation, every renamed replacement pin lands on the drawn pin it pairs with.
        /// Names the replacement lacks place nothing (negative space).
        #[test]
        fn instance_fits_land_every_pair(
            pins in prop::collection::btree_set((-50i32..50, -50i32..50), 2..6),
            shift in (-30i32..30, -30i32..30),
            at in (-500i32..500, -500i32..500),
            oi in 0usize..8,
        ) {
            let o = ORIENTS[oi];
            let at = [at.0 * 1000, at.1 * 1000];
            let old_at: Vec<[i32; 2]> = pins.iter().map(|(x, y)| [x * 1000, y * 1000]).collect();
            let new_pins: Vec<(String, [i32; 2])> = old_at
                .iter()
                .enumerate()
                .map(|(k, p)| (format!("p{k}"), [p[0] - shift.0 * 1000, p[1] - shift.1 * 1000]))
                .collect();
            let pairs: Vec<(usize, String)> = (0..old_at.len()).map(|k| (k, format!("p{k}"))).collect();
            let origin = fit_instance(&old_at, &pairs, &new_pins, o, at).expect("a moved master fits");
            for (k, name) in &pairs {
                let new = new_pins.iter().find(|(n, _)| n == name).unwrap().1;
                prop_assert_eq!(place(new, o, origin), place(old_at[*k], o, at));
            }
            let unknown: Vec<(usize, String)> = (0..old_at.len()).map(|k| (k, format!("q{k}"))).collect();
            prop_assert_eq!(fit_instance(&old_at, &unknown, &new_pins, o, at), None);
        }

        /// Any pattern moved by any translation, in any orientation, fits back exactly; one
        /// pin moved off the pattern no longer lands (negative space).
        #[test]
        fn moved_patterns_fit_back(
            pins in prop::collection::btree_set((-50i32..50, -50i32..50), 2..8),
            dx in -500i32..500,
            dy in -500i32..500,
            oi in 0usize..8,
            off in 1i32..40,
        ) {
            let o = ORIENTS[oi];
            let local: Vec<[i32; 2]> = pins.iter().map(|(x, y)| [x * 1000, y * 1000]).collect();
            let at = [dx * 1000, dy * 1000];
            let old: Vec<[i32; 2]> = local.iter().map(|p| place(*p, o, at).unwrap()).collect();
            let new: Vec<Option<[i32; 2]>> = local.iter().copied().map(Some).collect();
            let f = fit(&old, &new, o).expect("a moved pattern fits");
            prop_assert!(f.full());
            prop_assert_eq!(f.origin, at);
            let mut bad = new.clone();
            bad[0] = Some([local[0][0] + off * 1000, local[0][1]]);
            if let Some(f) = fit(&old, &bad, o) {
                prop_assert!(!f.full());
            }
        }
    }
}
