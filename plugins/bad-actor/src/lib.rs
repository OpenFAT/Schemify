//! Misbehaving plugin for the host's fault tests. Each `bad.*` command breaks one rule;
//! the host must survive every one with a declared error in its health report.
//!
//! Written against the raw ABI (no SDK) so it can lie about buffers and sizes.

use std::hint::black_box;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[link(wasm_import_module = "schemify")]
unsafe extern "C" {
    safe fn table_len(table: u32) -> i32;
    fn read_column(t: u32, c: u32, start: u32, count: u32, dst: u32, dst_len: u32) -> i32;
    fn log(level: u32, ptr: u32, len: u32);
    fn fs_read(path: u32, len: u32) -> i32;
    fn spawn(argv: u32, argv_len: u32, stdin: u32, stdin_len: u32) -> i32;
    fn fetch(url: u32, url_len: u32, body: u32, body_len: u32) -> i32;
    safe fn clipboard_get() -> i32;
}

static INPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static OUTPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static LIE_ALLOC: AtomicBool = AtomicBool::new(false);

const NETS: u32 = 4;
const NETS_PIN_COUNT: u32 = 3;

fn ptr(b: &[u8]) -> u32 {
    b.as_ptr() as u32
}

fn len(b: &[u8]) -> u32 {
    b.len() as u32
}

fn publish(bytes: Vec<u8>) -> u64 {
    let mut out = OUTPUT.lock().unwrap();
    *out = bytes;
    ((out.as_ptr() as u64) << 32) | out.len() as u64
}

#[unsafe(no_mangle)]
pub extern "C" fn schemify_abi() -> u32 {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn schemify_alloc(n: u32) -> u32 {
    if LIE_ALLOC.swap(false, Ordering::Relaxed) {
        return 0xFFFF_FF00;
    }
    let mut input = INPUT.lock().unwrap();
    *input = vec![0; n as usize];
    input.as_mut_ptr() as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn schemify_on_event(_ptr: u32, _len: u32) -> u64 {
    let input = INPUT.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&input).into_owned();
    if text.starts_with("event load") {
        return publish(b"contribute commands\n  | command bad.spin \"Spin\"\n".to_vec());
    }
    if text.starts_with("event rejected") {
        // Answer a rejection with more garbage; the host must not echo it back again.
        return publish(b"garbage again\n".to_vec());
    }
    let Some(rest) = text.strip_prefix("event command ") else {
        return 0;
    };
    let out: Vec<u8> = match rest.trim_end().split(' ').next().unwrap_or("") {
        "bad.spin" => {
            // Side effects first: none of them may survive the fuel abort.
            let path = b"lib/a.shcomp";
            let msg = b"spinning";
            // SAFETY: the host reads exactly these byte ranges of our memory.
            unsafe {
                fs_read(ptr(path), len(path));
                log(2, ptr(msg), len(msg));
            }
            let mut i = 0u64;
            loop {
                i = black_box(i).wrapping_add(1);
            }
        }
        "bad.garbage_ptr" => return (0xFFFF_0000u64 << 32) | 0x10,
        "bad.null_ptr" => return 5,
        "bad.huge_output" => return ((ptr(&input) as u64) << 32) | 0xFFFF_FFFF,
        "bad.garbage_bytes" => vec![
            0xff, 0xfe, b'\n', b'l', b'o', b'g', 0x80, b'\n', b' ', b'|', 0, 1, 2, b'\n',
        ],
        "bad.mixed" => concat!(
            "  orphan attr\n",
            "log info ok\n",
            "edit 999 commit x\n  | delete a\n",
            "frobnicate 1 2\n",
            "timer 1\n",
            "timer 2 999999999\n",
            "contribute nonsense\n  | x\n",
            "contribute ui\n  | command not_a_ui_word\n",
            "log info \"unterminated\n",
            "log info \"also ok\"\n",
        )
        .as_bytes()
        .to_vec(),
        "bad.deny" => {
            let argv = b"rm -rf /";
            let url = b"https://example.com";
            let escape = b"../secret";
            let mut dst = [0u8; 4];
            // SAFETY: every pointer is to a live local; the host writes at most 4 bytes to
            // `dst` and treats the bogus pointer as a range error, never touching it.
            let r = unsafe {
                [
                    spawn(ptr(argv), len(argv), 0, 0),
                    fetch(ptr(url), len(url), 0, 0),
                    clipboard_get(),
                    fs_read(ptr(escape), len(escape)),
                    read_column(99, 0, 0, 0, dst.as_mut_ptr() as u32, 4),
                    read_column(NETS, NETS_PIN_COUNT, 0, 1, 0xFFFF_FFF0, 4),
                    read_column(NETS, NETS_PIN_COUNT, 0, 1, dst.as_mut_ptr() as u32, 2),
                ]
            };
            format!("log info \"deny {r:?}\"\n").into_bytes()
        }
        "bad.grow" => {
            let before = core::arch::wasm32::memory_size(0);
            let grew = core::arch::wasm32::memory_grow(0, 1 << 14) as isize;
            format!(
                "log info \"grow {grew} {}\"\n",
                core::arch::wasm32::memory_size(0) == before
            )
            .into_bytes()
        }
        "bad.oom" => {
            black_box(Vec::<u8>::with_capacity(black_box(1 << 30)));
            Vec::new()
        }
        "bad.lie_alloc" => {
            LIE_ALLOC.store(true, Ordering::Relaxed);
            Vec::new()
        }
        "bad.recurse" => {
            black_box(recurse(black_box(0)));
            Vec::new()
        }
        "bad.flood" => "log debug x\n".repeat(2000).into_bytes(),
        _ => return 0,
    };
    publish(out)
}

fn recurse(n: u64) -> u64 {
    if black_box(true) {
        recurse(n + 1).wrapping_add(1)
    } else {
        n
    }
}

/// A "pure" function that reaches for the host anyway; the host must deny the read.
#[unsafe(no_mangle)]
pub extern "C" fn schemify_call(_fn_id: u32, _ptr: u32, _len: u32) -> u64 {
    publish(table_len(0).to_string().into_bytes())
}
