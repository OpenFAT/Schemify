//! The built guest (`crates/app/tests/fixtures/ota/plugins/gmid_lut.wasm`, from `plugins/build.sh`) driven
//! through its exports with wasmi, as the host does: Load → config → characterize → spawn →
//! wrdata of the real grid size → LUT written and functions re-declared → `schemify_call`. Checks
//! the I/O chain, the fuel each event costs against the default budget, and that the guest's
//! answer equals the native `eval` on the LUT it wrote.

use gmid_lut::lut::Lut;
use gmid_lut::{FN_STRIDE, Tables, chars, eval};
use schemify_abi::{Event, FUEL_DEFAULT, unpack};
use wasmi::{Caller, Config, Engine, Extern, Linker, Memory, Module, Store, TypedFunc};

const WASM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/app/tests/fixtures/ota/plugins/gmid_lut.wasm"
);

/// Requests the guest made: (import, project path or argv line, data).
#[derive(Default)]
struct Host {
    io: Vec<(&'static str, String, Vec<u8>)>,
}

fn bytes(c: &Caller<'_, Host>, ptr: u32, len: u32) -> Vec<u8> {
    let mem: Memory = c
        .get_export("memory")
        .and_then(Extern::into_memory)
        .expect("memory");
    let mut b = vec![0; len as usize];
    mem.read(c, ptr as usize, &mut b).expect("in bounds");
    b
}

fn record(c: &mut Caller<'_, Host>, op: &'static str, p: (u32, u32), d: (u32, u32)) -> i32 {
    let (path, data) = (bytes(c, p.0, p.1), bytes(c, d.0, d.1));
    let io = &mut c.data_mut().io;
    io.push((op, String::from_utf8(path).expect("utf-8"), data));
    io.len() as i32
}

struct Guest {
    store: Store<Host>,
    mem: Memory,
    alloc: TypedFunc<u32, u32>,
    on_event: TypedFunc<(u32, u32), u64>,
    call: TypedFunc<(u32, u32, u32), u64>,
}

impl Guest {
    fn load() -> Guest {
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let wasm = std::fs::read(WASM).expect("run plugins/build.sh first");
        assert!(wasm.len() < 200 * 1024, "wasm size {}", wasm.len());
        let module = Module::new(&engine, &wasm[..]).unwrap();
        let mut linker = Linker::<Host>::new(&engine);
        let m = "schemify";
        linker
            .func_wrap(m, "fs_read", |mut c: Caller<'_, Host>, p: u32, l: u32| {
                record(&mut c, "fs_read", (p, l), (0, 0))
            })
            .unwrap();
        linker
            .func_wrap(
                m,
                "fs_write",
                |mut c: Caller<'_, Host>, p: u32, l: u32, d: u32, dl: u32| {
                    record(&mut c, "fs_write", (p, l), (d, dl))
                },
            )
            .unwrap();
        linker
            .func_wrap(
                m,
                "spawn",
                |mut c: Caller<'_, Host>, p: u32, l: u32, d: u32, dl: u32| {
                    record(&mut c, "spawn", (p, l), (d, dl))
                },
            )
            .unwrap();
        let mut store = Store::new(&engine, Host::default());
        store.set_fuel(FUEL_DEFAULT).unwrap();
        let inst = linker.instantiate_and_start(&mut store, &module).unwrap();
        let mem = inst.get_memory(&store, "memory").unwrap();
        Guest {
            alloc: inst.get_typed_func(&store, "schemify_alloc").unwrap(),
            on_event: inst.get_typed_func(&store, "schemify_on_event").unwrap(),
            call: inst.get_typed_func(&store, "schemify_call").unwrap(),
            store,
            mem,
        }
    }

    fn input(&mut self, b: &[u8]) -> u32 {
        let ptr = self.alloc.call(&mut self.store, b.len() as u32).unwrap();
        self.mem.write(&mut self.store, ptr as usize, b).unwrap();
        ptr
    }

    fn output(&self, packed: u64) -> String {
        let (ptr, len) = unpack(packed);
        let mut out = vec![0; len as usize];
        if ptr != 0 {
            self.mem.read(&self.store, ptr as usize, &mut out).unwrap();
        }
        String::from_utf8(out).unwrap()
    }

    /// Delivers one event with a generous budget, then asserts it fit the host's default one;
    /// returns its output text and the fuel it used.
    fn event(&mut self, ev: &Event<'_>) -> (String, u64) {
        let mut b = Vec::new();
        ev.encode(&mut b).unwrap();
        self.store.set_fuel(100 * FUEL_DEFAULT).unwrap();
        let ptr = self.input(&b);
        let packed = self
            .on_event
            .call(&mut self.store, (ptr, b.len() as u32))
            .unwrap();
        let used = 100 * FUEL_DEFAULT - self.store.get_fuel().unwrap();
        assert!(used < FUEL_DEFAULT, "{} used {used} fuel", ev.kind_name());
        (self.output(packed), used)
    }

    fn done(&mut self, handle: usize, status: i32, bytes: &[u8]) -> (String, u64) {
        self.event(&Event::IoDone {
            handle: handle as u32,
            status,
            bytes,
        })
    }

    fn call(&mut self, fn_id: u32, args: &str) -> Result<String, u32> {
        self.store.set_fuel(FUEL_DEFAULT).unwrap();
        let ptr = self.input(args.as_bytes());
        let packed = self
            .call
            .call(&mut self.store, (fn_id, ptr, args.len() as u32))
            .unwrap();
        match unpack(packed) {
            (0, code) => Err(code),
            _ => Ok(self.output(packed)),
        }
    }
}

const CHARS: &str = "\
device nsim
  deck models/tt.spice
  inst \"X{n} d g s b nch W={w} L={l}\"
  op @m.x{n}.mnch
  vdd 1.8
  l 0.15 0.18 0.25 0.5 1 2 4 8
";

/// ngspice-shaped `wrdata` of instance `l` (µm) on the default 73 × 12 grid, from a smooth
/// weak-to-strong inversion model (the real files are this size).
fn wrdata(n: usize, l: f64) -> String {
    let mut t = String::from(" v-sweep");
    for q in ["id", "gm", "gds", "cgg"] {
        t.push_str(&format!(" @m.x{n}.mnch[{q}]"));
    }
    t.push('\n');
    for d in 1..=12 {
        let vds = 0.15 * f64::from(d);
        for g in 0..=72 {
            let vgs = 0.025 * f64::from(g);
            let u = (vgs - 0.45) / 0.0672;
            let f = u.exp().ln_1p();
            let clm = 1.0 + 0.015 * vds / l;
            let id = 2.4e-6 / l * f * f * clm;
            let gm = 2.4e-6 / l * 2.0 * f / (1.0 + (-u).exp()) / 0.0672 * clm;
            t.push_str(&format!(" {vgs:.8e}"));
            for x in [id, gm, id * 0.015 / l, 8.8e-15 * l] {
                t.push_str(&format!("  {x:.8e}"));
            }
            t.push('\n');
        }
    }
    t
}

fn io(g: &Guest, i: usize) -> (&'static str, String, Vec<u8>) {
    g.store.data().io[i].clone()
}

#[test]
fn guest_characterizes_and_answers_calls_within_the_fuel_budget() {
    let mut g = Guest::load();
    let (out, _) = g.event(&Event::Load);
    assert!(
        out.contains("contribute functions\n") && out.contains("function gmid.ft 6 3"),
        "{out}"
    );
    assert!(out.contains("command gmid.characterize") && out.contains("parent menu.tools"));
    assert_eq!(io(&g, 0).1, "gmid.chars");
    assert_eq!(
        g.call(1, "\"nsim\" 15 5e-7"),
        Err(6),
        "no table yet: E_RANGE"
    );

    g.done(1, 0, CHARS.as_bytes());
    assert_eq!(io(&g, 1).1, ".schemify/gmid/nsim.glut");
    // Load path (b): an existing LUT file (the committed nfet sample) under this device name.
    let sample = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/app/tests/fixtures/ota/gmid/nfet_01v8.glut"
    );
    let (out, _) = g.done(2, 0, &std::fs::read(sample).unwrap());
    assert!(
        out.contains("function gmid.jd 17 3") && out.contains("nsim loaded"),
        "{out}"
    );
    let jd: f64 = g.call(17, "\"nsim\" 15 5e-7").unwrap().parse().unwrap();
    assert!((1.0..10.0).contains(&jd), "sample jd {jd}");
    let (out, _) = g.done(2, 0, b"stale handle");
    assert!(out.is_empty(), "a completed handle is forgotten: {out}");

    // Path (a): characterize.
    g.event(&Event::Command {
        name: "gmid.characterize",
        args: "",
    });
    let (op, path, netlist) = io(&g, 2);
    assert_eq!(
        (op, path.as_str()),
        ("fs_write", ".schemify/gmid/work/nsim.sp")
    );
    assert!(
        String::from_utf8(netlist)
            .unwrap()
            .contains("X8 d g s b nch W=1 L=8\n")
    );
    g.done(3, 0, b"");
    let (op, argv, _) = io(&g, 3);
    assert_eq!(
        (op, argv.as_str()),
        ("spawn", "ngspice -b .schemify/gmid/work/nsim.sp")
    );
    g.done(4, 0, b"ngspice output");
    let sweep = chars::parse(CHARS).unwrap().remove(0).sweep.unwrap();
    let mut worst = 0;
    for (k, l) in sweep.l_um.iter().enumerate() {
        assert_eq!(
            io(&g, 4 + k).1,
            format!(".schemify/gmid/work/nsim.{}.data", k + 1)
        );
        let (out, fuel) = g.done(5 + k, 0, wrdata(k + 1, *l).as_bytes());
        worst = worst.max(fuel);
        let last = k + 1 == sweep.l_um.len();
        assert_eq!(
            out.contains("nsim characterized (ngspice): 8 L × 12 VDS × 73 VGS"),
            last,
            "{out}"
        );
        assert_eq!(
            out.contains("function gmid.jd 33 3"),
            last,
            "re-declared at generation 2"
        );
    }
    eprintln!("worst characterization event: {worst} fuel (budget {FUEL_DEFAULT})");
    assert!(worst < FUEL_DEFAULT / 3, "{worst}");
    let (op, path, file) = io(&g, 12);
    assert_eq!(
        (op, path.as_str()),
        ("fs_write", ".schemify/gmid/nsim.glut")
    );
    let mut t = Tables::default();
    t.insert("nsim", Lut::decode(&file).unwrap()).unwrap();
    let (out, _) = g.done(13, 0, b"");
    assert!(out.contains("nsim: LUT saved"), "{out}");

    for (f, args) in [
        (1, "\"nsim\" 15 5e-7"),
        (2, "\"nsim\" 12 7e-7 2e-5"),
        (6, "\"nsim\" 10 3e-6"),
    ] {
        let want = eval(&t, f, args).unwrap();
        let got: f64 = g.call(f + 2 * FN_STRIDE, args).unwrap().parse().unwrap();
        // The guest's libm (powf, ln) is not the host's: last-bit differences only. Within the
        // guest every platform runs the same code, so plugin results stay bit-identical (D20).
        assert!(
            (got - want).abs() <= want.abs() * 1e-12,
            "fn {f}({args}): {got} vs {want}"
        );
    }
    assert_eq!(g.call(1, "\"nsim\" 15 1e-3"), Err(6), "L out of range");
    assert_eq!(g.call(1, "garbage"), Err(6));

    // A failed run logs and leaves the table alone.
    g.event(&Event::Command {
        name: "gmid.characterize",
        args: "nsim",
    });
    g.done(14, 0, b"");
    let (out, _) = g.done(15, 1, b"... Error: could not find a valid modelname");
    assert!(
        out.contains("ngspice exited 1") && out.contains("valid modelname"),
        "{out}"
    );
    assert!(g.call(1, "\"nsim\" 15 5e-7").is_ok());
}

const ES_CHARS: &str = "\
device nes
  deck models/tt.spice
  inst \"M{n} d g s b nch w={w}u l={l}u\"
  sim espice
  vdd 0.8
  l 0.05 0.07 0.1 0.15 0.2 0.3 0.5 1
";

/// An ESPice binary raw file (the layout of `tests/fixtures/espice_op_ac.raw`) for `sweep`:
/// per `.step` point an `.op` plot and a one-point `.ac` plot, values from the smooth model of
/// [`wrdata`]. The real files are this size (≈ 2 MB for 8 lengths).
fn espice_raw(sweep: &chars::Sweep) -> Vec<u8> {
    let (vgs, vds) = chars::grid(sweep);
    let nl = sweep.l_um.len();
    let w = 2.0 * std::f64::consts::PI * chars::F_AC;
    let mut raw = Vec::new();
    for (k, (d, g)) in vds
        .iter()
        .flat_map(|d| vgs.iter().map(move |g| (*d, *g)))
        .enumerate()
    {
        for complex in [false, true] {
            let (kind, flags) = if complex {
                ("AC Analysis", "complex")
            } else {
                ("Operating Point", "real")
            };
            let mut head = format!(
                "Title: * gmid-lut espice sweep\nDate: Thu Jan  1 00:00:00 1970\n\
                 Plotname: {kind} (vds={d}, vgs={g})\nFlags: {flags}\nNo. Variables: {}\n\
                 No. Points: 1\nVariables:\n",
                3 * nl + usize::from(complex)
            );
            let mut v = 0;
            if complex {
                head.push_str("\t0\tfrequency\tfrequency\n");
                v = 1;
            }
            for n in 1..=nl {
                for a in ["vda", "vga", "vdb"] {
                    head.push_str(&format!("\t{v}\ti({a}{n})\tcurrent\n"));
                    v += 1;
                }
            }
            head.push_str("Binary:\n");
            raw.extend_from_slice(head.as_bytes());
            let mut push = |x: f64| raw.extend_from_slice(&x.to_le_bytes());
            if complex {
                push(chars::F_AC);
                push(0.0);
            }
            for &l in &sweep.l_um {
                let u = (g - 0.25) / 0.0672;
                let f = u.exp().ln_1p();
                let clm = 1.0 + 0.015 * d / l;
                let id = 2.4e-6 / l * f * f * clm + 1e-12 * (k + 1) as f64;
                let gm = 2.4e-6 / l * 2.0 * f / (1.0 + (-u).exp()) / 0.0672 * clm;
                if complex {
                    for (re, im) in [
                        (gm, -1e-9),
                        (1e-12, 8.8e-15 * l * w),
                        (id * 0.015 / l, 3e-9),
                    ] {
                        push(re);
                        push(im);
                    }
                } else {
                    for x in [id, 1e-12, id] {
                        push(x);
                    }
                }
            }
        }
    }
    raw
}

/// The ESPice path (README "The sweep"): `sim espice` spawns `espice … -r <raw>`, reads its
/// one binary raw file of the real size (8 L × 876 points) and builds the LUT inside one
/// event's fuel; a run whose raw file is missing logs and keeps the table.
#[test]
fn guest_characterizes_with_espice_within_the_fuel_budget() {
    let mut g = Guest::load();
    g.event(&Event::Load);
    g.done(1, 0, ES_CHARS.as_bytes());
    assert_eq!(io(&g, 1).1, ".schemify/gmid/nes.glut");
    g.done(2, -5, b"");
    g.event(&Event::Command {
        name: "gmid.characterize",
        args: "",
    });
    let (op, path, netlist) = io(&g, 2);
    assert_eq!(
        (op, path.as_str()),
        ("fs_write", ".schemify/gmid/work/nes.sp")
    );
    let netlist = String::from_utf8(netlist).unwrap();
    assert!(
        netlist.contains("Ma8 da8 ga8 s b nch w=1u l=1u\n") && netlist.contains(".step param vgs"),
        "{netlist}"
    );
    g.done(3, 0, b"");
    let (op, argv, _) = io(&g, 3);
    assert_eq!(op, "spawn");
    assert!(
        argv.starts_with("espice -b ")
            && argv.contains(".schemify/gmid/work/nes.raw")
            && argv.ends_with(" .schemify/gmid/work/nes.sp"),
        "{argv}"
    );
    g.done(4, 0, b"espice output");
    assert_eq!(
        (io(&g, 4).0, io(&g, 4).1.as_str()),
        ("fs_read", ".schemify/gmid/work/nes.raw")
    );
    let sweep = chars::parse(ES_CHARS).unwrap().remove(0).sweep.unwrap();
    let raw = espice_raw(&sweep);
    let (out, fuel) = g.done(5, 0, &raw);
    eprintln!(
        "espice raw file {} bytes parsed and built in {fuel} fuel (budget {FUEL_DEFAULT})",
        raw.len()
    );
    assert!(
        out.contains("nes characterized (espice): 8 L × 12 VDS × 73 VGS"),
        "{out}"
    );
    let (op, path, file) = io(&g, 5);
    assert_eq!((op, path.as_str()), ("fs_write", ".schemify/gmid/nes.glut"));
    let lut = Lut::decode(&file).unwrap();
    assert!(
        lut.provenance.contains("cgg includes overlap"),
        "{}",
        lut.provenance
    );
    let native = chars::build("nes", &sweep, &chars::parse_espice(&sweep, &raw).unwrap()).unwrap();
    assert_eq!(lut, native, "the guest built what the native code builds");

    // No raw file (ESPice failed before writing one): logged, the table stays.
    g.done(6, 0, b"");
    g.event(&Event::Command {
        name: "gmid.characterize",
        args: "nes",
    });
    g.done(7, 0, b"");
    g.done(8, 0, b"");
    let (out, _) = g.done(9, -5, b"");
    assert!(out.contains("espice wrote no raw file"), "{out}");
    assert!(g.call(1, "\"nes\" 15 5e-7").is_ok());
}
