//! Deterministic canvas fixtures shared by the golden, perf and GPU tests.

#![allow(dead_code)]

use schemify_render::canvas::CanvasPrims;

/// Deterministic 1k-primitive canvas covering every primitive kind, a selection mark and labels.
pub fn thousand_prims() -> CanvasPrims {
    let mut p = CanvasPrims::default();
    let mut seed = 0x9e37_79b9_u32;
    let mut rnd = |m: i32| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % m as u32) as i32
    };
    for _ in 0..600 {
        let (x, y) = (rnd(600) * 1000, rnd(380) * 1000);
        p.line(
            x,
            y,
            x + (rnd(80) - 40) * 1000,
            y + (rnd(80) - 40) * 1000,
            [1, 4, 5][rnd(3) as usize] as u8,
        );
    }
    for _ in 0..150 {
        let (x, y) = (rnd(580) * 1000, rnd(360) * 1000);
        p.rect(
            x,
            y,
            x + rnd(40) * 1000 + 1000,
            y + rnd(30) * 1000 + 1000,
            [4, 5][rnd(2) as usize] as u8,
        );
    }
    for _ in 0..120 {
        let sweep = if rnd(2) == 0 {
            360_000
        } else {
            90_000 + rnd(180) * 1000
        };
        p.arc(
            rnd(600) * 1000,
            rnd(380) * 1000,
            (rnd(20) + 3) * 1000,
            rnd(360) * 1000,
            sweep,
            4,
        );
    }
    for _ in 0..60 {
        let (x, y) = (rnd(560) * 1000, rnd(340) * 1000);
        let pts = [
            (x, y),
            (x + 20_000, y + rnd(30) * 1000),
            (x + 40_000, y),
            (x + 20_000, y - 15_000),
        ];
        p.poly(&pts, rnd(2) == 0, [1, 4, 5][rnd(3) as usize] as u8);
    }
    for i in 0..70 {
        p.text(
            rnd(560) * 1000,
            rnd(360) * 1000,
            12_000,
            if i % 2 == 0 { 3 } else { 1 },
            ["VPP", "VSS", "OUT", "IN", "x1"][i % 5],
        );
    }
    assert_eq!(p.len(), 1000);
    p
}

/// 100k primitives inside a 1600×900 view: 70k segments, 15k rects, 10k arcs, 5k polylines.
pub fn hundred_k() -> CanvasPrims {
    let mut p = CanvasPrims::default();
    let mut seed = 0x2545_f491_u32;
    let mut rnd = |m: u32| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % m) as i32
    };
    for _ in 0..70_000 {
        let (x, y) = (rnd(1_600_000), rnd(900_000));
        p.line(
            x,
            y,
            x + rnd(20_000) - 10_000,
            y + rnd(20_000) - 10_000,
            [1, 4][rnd(2) as usize] as u8,
        );
    }
    for _ in 0..15_000 {
        let (x, y) = (rnd(1_590_000), rnd(890_000));
        p.rect(x, y, x + rnd(10_000) + 500, y + rnd(10_000) + 500, 4);
    }
    for _ in 0..10_000 {
        p.arc(
            rnd(1_600_000),
            rnd(900_000),
            rnd(4_000) + 500,
            0,
            360_000,
            4,
        );
    }
    for _ in 0..5_000 {
        let (x, y) = (rnd(1_580_000), rnd(880_000));
        p.poly(
            &[
                (x, y),
                (x + 5_000, y + 8_000),
                (x + 10_000, y),
                (x + 15_000, y + 8_000),
            ],
            false,
            1,
        );
    }
    assert_eq!(p.len(), 100_000);
    p
}
