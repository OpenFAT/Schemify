//! Makes the sample LUTs: sky130 `nfet_01v8` and `pfet_01v8`, tt corner, 27 °C, VDD 1.8 V,
//! W = 1 µm, L at the model bins 0.15 … 8 µm. The same path the plugin takes —
//! `chars::parse` → `chars::netlist` → ngspice → `chars::parse_block` per L → `chars::build` →
//! `Lut::encode` — with `std::process` standing in for the host's `spawn`.
//!
//! ```text
//! cargo run --manifest-path plugins/Cargo.toml -p gmid-lut --example characterize -- \
//!     <sky130_fd_pr models root> <output dir>
//! ```

use std::path::Path;
use std::process::Command;

use gmid_lut::chars;
use gmid_lut::lut::Lut;
use gmid_lut::{Tables, eval};

const DEVICES: [(&str, char); 2] = [("nfet_01v8", 'n'), ("pfet_01v8", 'p')];

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [pdk, out] = args.as_slice() else {
        return Err("usage: characterize <sky130_fd_pr models root> <output dir>".to_owned());
    };
    let work = std::env::temp_dir().join("gmid-characterize");
    let io = |e: std::io::Error| e.to_string();
    std::fs::create_dir_all(work.join(chars::WORK_DIR)).map_err(io)?;
    std::fs::create_dir_all(out).map_err(io)?;
    let mut tables = Tables::default();
    for (dev, pol) in DEVICES {
        let cell = format!("{pdk}/cells/{dev}/sky130_fd_pr__{dev}");
        let deck = format!(
            "* sky130 {dev}, tt\n.option scale=1.0u\n.param mc_mm_switch=0 mc_pr_switch=0\n\
             .include \"{pdk}/models/parameters/lod.spice\"\n\
             .include \"{pdk}/models/corners/tt/nonfet.spice\"\n\
             .include \"{cell}__tt.corner.spice\"\n.include \"{cell}__mismatch.corner.spice\"\n"
        );
        std::fs::write(work.join(format!("{dev}.deck.spice")), deck).map_err(io)?;
        let config = format!(
            "device {dev}\n  deck {dev}.deck.spice\n  inst \"X{{n}} d g s b sky130_fd_pr__{dev} W={{w}} L={{l}} nf=1\"\n  \
             op @m.x{{n}}.msky130_fd_pr__{dev}\n  polarity {pol}\n  vdd 1.8\n  \
             l 0.15 0.18 0.25 0.5 1 2 4 8\n  w 1\n  note \"sky130_fd_pr tt, 27 C, ngspice\"\n"
        );
        let d = chars::parse(&config)?.remove(0);
        let sweep = d.sweep.as_ref().expect("config has a deck");
        std::fs::write(
            work.join(chars::netlist_path(dev)),
            chars::netlist(dev, sweep),
        )
        .map_err(io)?;
        let run = Command::new("ngspice")
            .args(["-b", &chars::netlist_path(dev)])
            .current_dir(&work)
            .output()
            .map_err(|e| format!("ngspice: {e}"))?;
        if !run.status.success() {
            return Err(format!(
                "{dev}: ngspice failed:\n{}",
                String::from_utf8_lossy(&run.stdout)
            ));
        }
        let mut blocks = Vec::new();
        for n in 1..=sweep.l_um.len() {
            let data = std::fs::read_to_string(work.join(chars::data_path(dev, n))).map_err(io)?;
            blocks.push(chars::parse_block(&data)?);
        }
        let lut = chars::build(dev, sweep, &blocks)?;
        let mut file = Vec::new();
        lut.encode(&mut file);
        assert_eq!(Lut::decode(&file).as_ref(), Ok(&lut), "round trip");
        let path = Path::new(out).join(format!("{dev}.glut"));
        std::fs::write(&path, &file).map_err(io)?;
        println!("{dev}: {} bytes -> {}", file.len(), path.display());
        for (li, l) in lut.l.iter().enumerate() {
            let (lo, hi) = lut.gm_id_range(li);
            println!("  L {:>5.2} um: gm/Id {lo:5.2} .. {hi:5.2} 1/V", l * 1e6);
        }
        tables.insert(dev, lut).map_err(|c| c.to_string())?;
        for (f, id) in [(1, ""), (0, ""), (5, ""), (6, ""), (2, " 2e-5")] {
            for g in [5, 10, 15, 20] {
                let a = format!("\"{dev}\" {g} 5e-7{id}");
                let name = gmid_lut::FUNCTIONS[f as usize].0;
                println!("  {name}({a}) = {:?}", eval(&tables, f, &a));
            }
        }
    }
    Ok(())
}
