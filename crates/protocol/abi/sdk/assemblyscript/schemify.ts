// schemify.ts: the Schemify plugin ABI v1 for AssemblyScript (docs/spec/plugin-abi.md).
//
// The plugin's entry file registers a handler and re-exports the module's exports (asc
// resolves a package import as node_modules/<package>/<path>.ts, hence the /schemify):
//
//     import { Event, EventKind, Out, onEvent } from "@schemify/sdk/schemify";
//     export { schemify_abi, schemify_alloc, schemify_on_event } from "@schemify/sdk/schemify";
//     onEvent((ev: Event, out: Out): void => { ... });
//
// (AssemblyScript turns only the entry file's exports into wasm exports.) Build with
// "runtime": "stub" and "use": ["abort="] (asconfig.json), so the module imports nothing
// but module "schemify". The stub runtime never frees: this library allocates only while
// the start function runs, and a handler should write into Out buffers rather than build
// strings with `+` or toString(), each of which leaks until the plugin unloads.
//
// Buffers are static: an event over INPUT_MAX bytes is refused (the host reports E_SPACE),
// and output past OUTPUT_MAX is replaced by one error log.

// ---- ABI numbers, from crates/protocol/abi (cargo test -p schemify-abi --test sdk) ----
// BEGIN GENERATED
export const ABI_VERSION: u32 = 1;

/** Error codes (ABI §8); imports return them negated. */
export enum Code {
  Denied = 1,
  Gone = 2,
  Space = 3,
  Table = 4,
  Column = 5,
  Range = 6,
  Parse = 7,
  Fuel = 8,
  Version = 9,
  Trap = 10,
  Module = 11,
}
/** Wire names of `Code`, indexed by the code. */
export const CODE_NAMES: StaticArray<string> = ["", "E_DENIED", "E_GONE", "E_SPACE", "E_TABLE", "E_COLUMN", "E_RANGE", "E_PARSE", "E_FUEL", "E_VERSION", "E_TRAP", "E_MODULE"];

/** Snapshot tables (ABI §4). */
export enum Table {
  Docs = 0,
  Instances = 1,
  Wires = 2,
  Labels = 3,
  Nets = 4,
  InstParams = 5,
  Paths = 6,
  NameMap = 7,
  Violations = 8,
  DocVars = 9,
  BenchSpecs = 10,
  BenchOutputs = 11,
  Results = 12,
  Selection = 13,
  UiNodes = 14,
}

/** Column ids, <TABLE>_<COLUMN>. */
export const DOCS_HANDLE: u32 = 0; // u32
export const DOCS_KIND: u32 = 1; // u8
export const DOCS_DIRTY: u32 = 2; // u8
export const DOCS_PATH: u32 = 3; // str
export const DOCS_CELL: u32 = 4; // str
export const INSTANCES_DOC: u32 = 0; // u32
export const INSTANCES_ID: u32 = 1; // u32
export const INSTANCES_X: u32 = 2; // i32
export const INSTANCES_Y: u32 = 3; // i32
export const INSTANCES_ORIENT: u32 = 4; // u8
export const INSTANCES_CELL: u32 = 5; // str
export const INSTANCES_NAME: u32 = 6; // str
export const WIRES_DOC: u32 = 0; // u32
export const WIRES_ID: u32 = 1; // u32
export const WIRES_X1: u32 = 2; // i32
export const WIRES_Y1: u32 = 3; // i32
export const WIRES_X2: u32 = 4; // i32
export const WIRES_Y2: u32 = 5; // i32
export const WIRES_NET: u32 = 6; // u32
export const LABELS_DOC: u32 = 0; // u32
export const LABELS_ID: u32 = 1; // u32
export const LABELS_X: u32 = 2; // i32
export const LABELS_Y: u32 = 3; // i32
export const LABELS_NET: u32 = 4; // u32
export const LABELS_TEXT: u32 = 5; // str
export const NETS_NAME: u32 = 0; // str
export const NETS_WIDTH: u32 = 1; // u16
export const NETS_GLOBAL: u32 = 2; // u8
export const NETS_PIN_COUNT: u32 = 3; // u32
export const INST_PARAMS_INST_ROW: u32 = 0; // u32
export const INST_PARAMS_NAME: u32 = 1; // str
export const INST_PARAMS_VALUE: u32 = 2; // str
export const INST_PARAMS_SOURCE: u32 = 3; // u8
export const PATHS_PATH: u32 = 0; // str
export const PATHS_CELL: u32 = 1; // str
export const PATHS_IMPL: u32 = 2; // str
export const PATHS_PARENT: u32 = 3; // u32
export const NAME_MAP_PATH: u32 = 0; // str
export const NAME_MAP_EMITTED: u32 = 1; // str
export const VIOLATIONS_CODE: u32 = 0; // str
export const VIOLATIONS_SEVERITY: u32 = 1; // u8
export const VIOLATIONS_OBJECT: u32 = 2; // u32
export const VIOLATIONS_PARAM: u32 = 3; // str
export const DOC_VARS_NAME: u32 = 0; // str
export const DOC_VARS_EXPR: u32 = 1; // str
export const DOC_VARS_VALUE: u32 = 2; // str
export const BENCH_SPECS_NAME: u32 = 0; // str
export const BENCH_SPECS_EXPR: u32 = 1; // str
export const BENCH_SPECS_MIN: u32 = 2; // str
export const BENCH_SPECS_MAX: u32 = 3; // str
export const BENCH_OUTPUTS_NAME: u32 = 0; // str
export const BENCH_OUTPUTS_EXPR: u32 = 1; // str
export const BENCH_OUTPUTS_MIN: u32 = 2; // str
export const BENCH_OUTPUTS_MAX: u32 = 3; // str
export const RESULTS_JOB: u32 = 0; // u32
export const RESULTS_OUTPUT: u32 = 1; // str
export const RESULTS_VALUE: u32 = 2; // str
export const RESULTS_PASS: u32 = 3; // u8
export const SELECTION_DOC: u32 = 0; // u32
export const SELECTION_ID: u32 = 1; // u32
export const UI_NODES_ID: u32 = 0; // str
export const UI_NODES_KIND: u32 = 1; // u8
export const UI_NODES_PARENT: u32 = 2; // str
export const UI_NODES_OWNER: u32 = 3; // u16

/** Log levels. */
export enum Level {
  Error = 0,
  Warn = 1,
  Info = 2,
  Debug = 3,
}

/** Contribution kinds (ABI §1). */
export const KIND_UI = "ui";
export const KIND_COMMANDS = "commands";
export const KIND_KEYMAP = "keymap";
export const KIND_ANNOTATIONS = "annotations";
export const KIND_FUNCTIONS = "functions";
export const KIND_DIALECTS = "dialects";
export const KIND_RUNNERS = "runners";
export const KIND_CHECKS = "checks";
export const KIND_CONSTRAINT_TYPES = "constraint_types";
// END GENERATED

/** Bytes of an event over this are refused; the host reports E_SPACE. */
export const INPUT_MAX: u32 = 1 << 20;
/** Output past this is dropped and replaced by one `log error` record. */
export const OUTPUT_MAX: u32 = 1 << 20;
/** A string argument of an import wrapper is encoded here first; past this, -E_SPACE. */
export const SCRATCH_MAX: u32 = 1 << 20;

// ---- Strings: borrowed UTF-8 bytes ----

/** UTF-8 bytes in linear memory, never owned and never NUL-terminated. An event's
 *  strings point into the input buffer and are valid only until the handler returns.
 *  Also a cursor: nextValue and nextString consume it from the front. */
export class Str {
  ptr: usize;
  len: u32;

  constructor(ptr: usize = 0, len: u32 = 0) {
    this.ptr = ptr;
    this.len = len;
  }

  /** Points this at `len` bytes at `ptr`; returns this. */
  set(ptr: usize, len: u32): Str {
    this.ptr = ptr;
    this.len = len;
    return this;
  }

  /** Byte `i`, unchecked: `i` must be below len. */
  byte(i: u32): u8 {
    return load<u8>(this.ptr + i);
  }

  /** True when the bytes equal `other`, a Str or a string compared as its UTF-8. */
  eq<T>(other: T): bool {
    if (!isString<T>()) {
      const o = changetype<Str>(other);
      return o.len == this.len && memory.compare(o.ptr, this.ptr, this.len) == 0;
    }
    const s = changetype<string>(other);
    let k: u32 = 0;
    for (let i = 0; i < s.length; ) {
      const cp = cpAt(s, i);
      i += cp >= 0x10000 ? 2 : 1;
      const n = cpLen(cp);
      if (n > this.len - k) return false;
      for (let j: u32 = 0; j < n; j++) if (this.byte(k + j) != cpByte(cp, n, j)) return false;
      k += n;
    }
    return k == this.len;
  }

  /** A new string holding the bytes. Allocates, and the stub runtime never frees it. */
  toString(): string {
    return String.UTF8.decodeUnsafe(this.ptr, this.len);
  }

  /** Copies the next line-record value (format spec §2: bare, or quoted with \" \\ \n \t
   *  \u{hex} escapes) unescaped into `dst`, replacing its text. Returns its length, -1
   *  when none is left, or -Code.Parse when malformed or longer than dst's capacity,
   *  after which this is empty. */
  nextValue(dst: Out): i32 {
    dst.clear();
    const s = this.ptr;
    const len = this.len;
    const bad = -Code.Parse;
    let i: u32 = 0;
    while (i < len && isSpace(load<u8>(s + i))) i++;
    if (i == len) {
      this.set(s + i, 0);
      return -1;
    }
    this.len = 0; // stop after an error
    let c = load<u8>(s + i);
    if (c == 0x22) {
      for (i++; ; i++) {
        if (i == len) return bad;
        c = load<u8>(s + i);
        if (c == 10 || c == 13) return bad;
        if (c == 0x22) break;
        if (c != 0x5c) {
          if (!pushByte(dst, c)) return bad;
          continue;
        }
        if (++i == len) return bad;
        const e = load<u8>(s + i);
        let cp: u32 = 0;
        if (e == 0x22 || e == 0x5c) cp = e;
        else if (e == 0x6e) cp = 10;
        else if (e == 0x74) cp = 9;
        else if (e == 0x75 && i + 1 < len && load<u8>(s + i + 1) == 0x7b) {
          let digits = 0;
          for (i += 2; i < len && load<u8>(s + i) != 0x7d; i++, digits++) {
            const h = hexValue(load<u8>(s + i));
            if (digits == 6 || h == 16) return bad;
            cp = cp * 16 + h;
          }
          if (i == len || digits == 0 || cp > 0x10ffff || (cp >= 0xd800 && cp < 0xe000)) return bad;
        } else return bad;
        if (!pushCp(dst, cp)) return bad;
      }
      i++;
    } else {
      if (c == 0x23 || c == 0x7c) return bad;
      for (; i < len && isBareByte(load<u8>(s + i)); i++) if (!pushByte(dst, load<u8>(s + i))) return bad;
    }
    if (i < len && !isSpace(load<u8>(s + i))) return bad;
    this.set(s + i, len - i);
    return <i32>dst.len;
  }

  /** Points `s` at the next string of a readStrings buffer (u32 LE length + bytes each)
   *  and consumes it; false when none is left or the rest is truncated. */
  nextString(s: Str): bool {
    if (this.len < 4) return false;
    const n = load<u32>(this.ptr);
    if (n > this.len - 4) return false;
    s.set(this.ptr + 4, n);
    this.set(this.ptr + 4 + n, this.len - 4 - n);
    return true;
  }
}

// ---- Output (ABI §7) ----

/** An append-only text buffer over static memory: the event's output records, or any text
 *  you compose (a log message, an edit's ops). Once full, `overflow` is set and appends
 *  are dropped. Every put takes a string (written as UTF-8; a lone surrogate as U+FFFD),
 *  a Str (bytes verbatim) or, for put, an integer (decimal). */
export class Out extends Str {
  cap: u32;
  overflow: bool = false;

  /** A buffer of `cap` bytes at `ptr`, e.g. `new Out(memory.data(N), N)` at top level,
   *  so the object is allocated once, by the start function. */
  constructor(ptr: usize, cap: u32) {
    super(ptr, 0);
    this.cap = cap;
  }

  /** Empties the buffer and clears `overflow`. */
  clear(): Out {
    this.len = 0;
    this.overflow = false;
    return this;
  }

  /** Raw text, or an integer in decimal. */
  put<T>(v: T): Out {
    if (isInteger<T>()) {
      let u = <u64>v;
      if (isSigned<T>() && <i64>v < 0) {
        this.putByte(0x2d);
        u = 0 - u;
      }
      let d: u32 = 1;
      for (let t = u; t >= 10; t /= 10) d++;
      if (!fits(this, d)) return this;
      for (let k = d; k > 0; k--) {
        store<u8>(this.ptr + this.len + k - 1, <u8>(0x30 + u % 10));
        u /= 10;
      }
      this.len += d;
      return this;
    }
    putRange(this, v, 0, count(v));
    return this;
  }

  /** One raw byte. */
  putByte(b: u8): Out {
    if (fits(this, 1)) store<u8>(this.ptr + this.len++, b);
    return this;
  }

  /** One line-record value: bare when legal, else quoted with \" \\ \n \t escaped and
   *  other control characters as \u{x} (lowercase hex, no leading zeros). */
  putValue<T>(v: T): Out {
    const n = count(v);
    if (isBareText(v, n)) return this.put(v);
    this.putByte(0x22);
    for (let i = 0; i < n; ) {
      const c = unitAt(v, i);
      if (c >= 0x80) { // never escaped: copy the whole non-ASCII run
        let j = i + 1;
        while (j < n && unitAt(v, j) >= 0x80) j++;
        putRange(this, v, i, j);
        i = j;
        continue;
      }
      i++;
      if (c == 0x22) this.put('\\"');
      else if (c == 0x5c) this.put("\\\\");
      else if (c == 10) this.put("\\n");
      else if (c == 9) this.put("\\t");
      else if (c < 0x20 || c == 0x7f) {
        this.put("\\u{");
        if (c >= 0x10) this.putByte(HEX[c >> 4]);
        this.putByte(HEX[c & 15]).putByte(0x7d);
      } else this.putByte(<u8>c);
    }
    return this.putByte(0x22);
  }

  /** `contribute <kind>`: replaces this plugin's set of `kind` (a KIND_* constant) with
   *  `body`, line-record text with one record per line, written verbatim. */
  contribute<T>(kind: string, body: T): Out {
    this.put("contribute ").put(kind).putByte(10);
    verbatim(this, body);
    return this;
  }

  /** contribute for a kind with a scope (annotations). */
  contributeScoped<S, T>(kind: string, scope: S, body: T): Out {
    this.put("contribute ").put(kind).putByte(0x20).putValue(scope).putByte(10);
    verbatim(this, body);
    return this;
  }

  /** A staged edit of `doc`: the user reviews `ops` (edits spec §1, one per line) first. */
  stage<L, T>(doc: u32, label: L, ops: T): Out {
    return this.edit(doc, "stage", label, ops);
  }

  /** An edit of `doc` applied at once, one undo step. */
  commit<L, T>(doc: u32, label: L, ops: T): Out {
    return this.edit(doc, "commit", label, ops);
  }

  /** A simulation job; follow it with vary records for its variables. */
  run<B, C, K>(bench: B, config: C, corner: K): Out {
    this.put("run ").putValue(bench).putByte(0x20).putValue(config).putByte(0x20);
    return this.putValue(corner).putByte(10);
  }

  /** Sets `name` to `value` in the run written just before. */
  vary<N, V>(name: N, value: V): Out {
    return this.put("  vary ").putValue(name).putByte(0x20).putValue(value).putByte(10);
  }

  /** Asks for a timer event `id` after `ms` (at most 86 400 000). */
  timer(id: u32, ms: u32): Out {
    return this.put("timer ").put(id).putByte(0x20).put(ms).putByte(10);
  }

  /** A log line, queued with the other outputs (hostLog writes at once). */
  log<T>(level: Level, text: T): Out {
    return this.put("log ").put(LEVEL_NAMES[level & 3]).putByte(0x20).putValue(text).putByte(10);
  }

  private edit<L, T>(doc: u32, mode: string, label: L, ops: T): Out {
    this.put("edit ").put(doc).putByte(0x20).put(mode).putByte(0x20).putValue(label).putByte(10);
    verbatim(this, ops);
    return this;
  }
}

const LEVEL_NAMES: StaticArray<string> = ["error", "warn", "info", "debug"];
const HEX: StaticArray<u8> = [48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 97, 98, 99, 100, 101, 102];

// Text helpers over a string (UTF-16 units) or a Str (bytes); ASCII is the same in both.

function count<T>(v: T): i32 {
  if (!isReference<T>()) ERROR("expected a string or a Str");
  if (isString<T>()) return changetype<string>(v).length;
  return <i32>changetype<Str>(v).len;
}

function unitAt<T>(v: T, i: i32): u32 {
  if (isString<T>()) return <u32>changetype<string>(v).charCodeAt(i);
  return load<u8>(changetype<Str>(v).ptr + i);
}

// Format §2: a bare value is non-empty, starts with neither # nor |, and has no
// whitespace, quote, backslash or control character.
function isBareText<T>(v: T, n: i32): bool {
  if (n == 0) return false;
  const first = unitAt(v, 0);
  if (first == 0x23 || first == 0x7c) return false;
  for (let i = 0; i < n; i++) if (!isBareByte(unitAt(v, i))) return false;
  return true;
}

function isBareByte(c: u32): bool {
  return c > 0x20 && c != 0x7f && c != 0x22 && c != 0x5c;
}

function isSpace(c: u32): bool {
  return c == 0x20 || c == 9;
}

function hexValue(c: u32): u32 {
  if (c >= 0x30 && c <= 0x39) return c - 0x30;
  if (c >= 0x61 && c <= 0x66) return c - 0x61 + 10;
  if (c >= 0x41 && c <= 0x46) return c - 0x41 + 10;
  return 16;
}

// The code point at unit i of s; a lone surrogate reads as U+FFFD.
function cpAt(s: string, i: i32): u32 {
  const u = <u32>s.charCodeAt(i);
  if (u - 0xd800 >= 0x800) return u;
  if (u < 0xdc00 && i + 1 < s.length) {
    const l = <u32>s.charCodeAt(i + 1);
    if (l - 0xdc00 < 0x400) return 0x10000 + ((u - 0xd800) << 10) + (l - 0xdc00);
  }
  return 0xfffd;
}

function cpLen(cp: u32): u32 {
  return cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
}

// Byte k of cp's n-byte UTF-8 encoding.
function cpByte(cp: u32, n: u32, k: u32): u8 {
  if (n == 1) return <u8>cp;
  if (k > 0) return <u8>(0x80 | ((cp >> (6 * (n - 1 - k))) & 0x3f));
  const lead: u32 = n == 2 ? 0xc0 : n == 3 ? 0xe0 : 0xf0;
  return <u8>(lead | (cp >> (6 * (n - 1))));
}

// Room for n more bytes in o; otherwise sets overflow.
function fits(o: Out, n: u32): bool {
  if (o.overflow || n > o.cap - o.len) {
    o.overflow = true;
    return false;
  }
  return true;
}

// Appends one byte if it fits, without touching overflow.
function pushByte(o: Out, b: u8): bool {
  if (o.len == o.cap) return false;
  store<u8>(o.ptr + o.len++, b);
  return true;
}

// Appends cp as UTF-8 if it fits, without touching overflow.
function pushCp(o: Out, cp: u32): bool {
  const n = cpLen(cp);
  if (n > o.cap - o.len) return false;
  for (let k: u32 = 0; k < n; k++) store<u8>(o.ptr + o.len + k, cpByte(cp, n, k));
  o.len += n;
  return true;
}

// Units [a, b) of v as UTF-8.
function putRange<T>(o: Out, v: T, a: i32, b: i32): void {
  if (isString<T>()) {
    const s = changetype<string>(v);
    for (let i = a; i < b; ) {
      const cp = cpAt(s, i);
      i += cp >= 0x10000 ? 2 : 1;
      if (!fits(o, cpLen(cp))) return;
      pushCp(o, cp);
    }
  } else {
    const n = <u32>(b - a);
    if (!fits(o, n)) return;
    memory.copy(o.ptr + o.len, changetype<Str>(v).ptr + a, n);
    o.len += n;
  }
}

// Each line of body as `  | line` (a trailing CR dropped); an empty line as `  |`.
function verbatim<T>(o: Out, body: T): void {
  const n = count(body);
  let start = 0;
  for (let i = 0; i <= n; i++) {
    if (i < n && unitAt(body, i) != 10) continue;
    if (i == n && start == i) break;
    let end = i;
    if (end > start && unitAt(body, end - 1) == 13) end--;
    o.put(end > start ? "  | " : "  |");
    putRange(o, body, start, end);
    o.putByte(10);
    start = i + 1;
  }
}

// ---- Events (ABI §6) ----

export enum EventKind {
  Load,
  Unload,
  Command,
  DocOpened,
  DocClosed,
  DocChanged,
  SelectionChanged,
  NetlistRequest,
  RunJob,
  RunDone,
  IoDone,
  Timer,
  Saved,
  Rejected,
}

/** One event; the fields its kind uses are set, the rest are zero or empty. The handler
 *  gets the same object every time: copy out what must outlive the event. */
export class Event {
  kind: EventKind = EventKind.Load;
  /** command */
  name: Str = new Str();
  /** command: line-record values, walk them with args.nextValue */
  args: Str = new Str();
  /** netlist_request */
  dialect: Str = new Str();
  /** netlist_request */
  bench: Str = new Str();
  /** run_job */
  lang: Str = new Str();
  /** doc_opened, doc_closed, doc_changed, saved */
  doc: u32 = 0;
  /** doc_changed */
  epoch: u32 = 0;
  /** run_job, run_done */
  job: u32 = 0;
  /** io_done */
  handle: u32 = 0;
  /** io_done: >= 0 success (the exit status for spawn), < 0 a negated Code */
  status: i32 = 0;
  /** timer */
  id: u32 = 0;
  /** rejected: a Code */
  code: u32 = 0;
  /** io_done payload, rejected record (raw bytes after the NUL) */
  bytes: Str = new Str();
}

// The unread rest of the event line, and its last token.
let cur: usize = 0;
let curEnd: usize = 0;
let num: i64 = 0;
const TOKEN = new Str();

function skipSpace(): void {
  while (cur < curEnd && isSpace(load<u8>(cur))) cur++;
}

function token(): Str {
  skipSpace();
  const start = cur;
  while (cur < curEnd && !isSpace(load<u8>(cur))) cur++;
  return TOKEN.set(start, <u32>(cur - start));
}

// The next token as a decimal integer in [lo, hi], into num.
function number(lo: i64, hi: i64): bool {
  const t = token();
  const neg = t.len > 0 && t.byte(0) == 0x2d;
  let i: u32 = neg ? 1 : 0;
  if (i == t.len) return false;
  let n: i64 = 0;
  for (; i < t.len; i++) {
    const c = t.byte(i);
    if (c < 0x30 || c > 0x39 || n > <i64>1 << 40) return false;
    n = n * 10 + (c - 0x30);
  }
  if (neg) n = -n;
  num = n;
  return n >= lo && n <= hi;
}

function u32Field(): bool {
  return number(0, 0xffffffff);
}

function ident(dst: Str): bool {
  const t = token();
  dst.set(t.ptr, t.len);
  return isBareText(dst, dst.len);
}

/** Parses the wire form at `ptr` into `ev`; false when malformed. Strings borrow the
 *  input bytes. */
export function parseEvent(ev: Event, ptr: usize, len: u32): bool {
  ev.kind = EventKind.Load;
  ev.name.set(0, 0);
  ev.args.set(0, 0);
  ev.dialect.set(0, 0);
  ev.bench.set(0, 0);
  ev.lang.set(0, 0);
  ev.bytes.set(0, 0);
  ev.doc = ev.epoch = ev.job = ev.handle = ev.id = ev.code = 0;
  ev.status = 0;
  let line = len;
  let payload = false;
  for (let i: u32 = 0; i < len; i++) {
    if (load<u8>(ptr + i) != 0) continue;
    line = i;
    ev.bytes.set(ptr + i + 1, len - i - 1);
    payload = true;
    break;
  }
  if (line > 0 && load<u8>(ptr + line - 1) == 10) line--;
  for (let i: u32 = 0; i < line; i++) {
    const c = load<u8>(ptr + i);
    if (c == 10 || c == 13) return false;
  }
  cur = ptr;
  curEnd = ptr + line;
  if (!token().eq("event")) return false;
  const k = token(); // valid until the next token()
  let ok = true;
  let wantsPayload = false;
  if (k.eq("load")) ev.kind = EventKind.Load;
  else if (k.eq("unload")) ev.kind = EventKind.Unload;
  else if (k.eq("selection_changed")) ev.kind = EventKind.SelectionChanged;
  else if (k.eq("command")) {
    ev.kind = EventKind.Command;
    ok = ident(ev.name);
    skipSpace();
    ev.args.set(cur, <u32>(curEnd - cur));
    cur = curEnd;
  } else if (k.eq("doc_opened") || k.eq("doc_closed") || k.eq("saved")) {
    ev.kind = k.eq("saved") ? EventKind.Saved : k.eq("doc_opened") ? EventKind.DocOpened : EventKind.DocClosed;
    ok = u32Field();
    ev.doc = <u32>num;
  } else if (k.eq("doc_changed")) {
    ev.kind = EventKind.DocChanged;
    ok = u32Field();
    ev.doc = <u32>num;
    ok = ok && u32Field();
    ev.epoch = <u32>num;
  } else if (k.eq("netlist_request")) {
    ev.kind = EventKind.NetlistRequest;
    ok = ident(ev.dialect) && ident(ev.bench);
  } else if (k.eq("run_job")) {
    ev.kind = EventKind.RunJob;
    ok = u32Field();
    ev.job = <u32>num;
    ok = ok && ident(ev.lang);
  } else if (k.eq("run_done")) {
    ev.kind = EventKind.RunDone;
    ok = u32Field();
    ev.job = <u32>num;
  } else if (k.eq("timer")) {
    ev.kind = EventKind.Timer;
    ok = u32Field();
    ev.id = <u32>num;
  } else if (k.eq("io_done")) {
    ev.kind = EventKind.IoDone;
    wantsPayload = true;
    ok = u32Field();
    ev.handle = <u32>num;
    ok = ok && number(-0x80000000, 0x7fffffff);
    ev.status = <i32>num;
  } else if (k.eq("rejected")) {
    ev.kind = EventKind.Rejected;
    wantsPayload = true;
    const name = token();
    for (let c = 1; c < CODE_NAMES.length; c++) if (name.eq(CODE_NAMES[c])) ev.code = c;
    ok = ev.code != 0;
  } else return false;
  return ok && payload == wantsPayload && token().len == 0;
}

// ---- Host imports (ABI §3, §5). Negative returns are a negated Code. ----

/** Row count of a snapshot table. */
@external("schemify", "table_len")
export declare function tableLen(table: u32): i32;

/** Changes whenever the table changes (31-bit, wraps): a cheap staleness check. */
@external("schemify", "table_epoch")
export declare function tableEpoch(table: u32): i32;

/** Copies `count` rows of a fixed-width column, little-endian, to `dst` (dstLen bytes);
 *  returns bytes written. -Code.Column for a string column. */
@external("schemify", "read_column")
export declare function readColumn(table: u32, column: u32, start: u32, count: u32, dst: usize, dstLen: u32): i32;

/** Copies `count` strings as u32 length + UTF-8 bytes each; walk them with
 *  `new Str(dst, written).nextString`. On -Code.Space the needed size is in dst's first 4
 *  bytes. */
@external("schemify", "read_strings")
export declare function readStrings(table: u32, column: u32, start: u32, count: u32, dst: usize, dstLen: u32): i32;

/** The clipboard's text (grant `clipboard`): returns a handle; the text arrives as io_done. */
@external("schemify", "clipboard_get")
export declare function clipboardGet(): i32;

@external("schemify", "eval")
declare function eval_(doc: u32, ptr: usize, len: u32): i32;
@external("schemify", "log")
declare function log_(level: u32, ptr: usize, len: u32): void;
@external("schemify", "fs_read")
declare function fs_read(path: usize, pathLen: u32): i32;
@external("schemify", "fs_list")
declare function fs_list(path: usize, pathLen: u32): i32;
@external("schemify", "fs_write")
declare function fs_write(path: usize, pathLen: u32, data: usize, dataLen: u32): i32;
@external("schemify", "fs_remove")
declare function fs_remove(path: usize, pathLen: u32): i32;
@external("schemify", "spawn")
declare function spawn_(argv: usize, argvLen: u32, stdin: usize, stdinLen: u32): i32;
@external("schemify", "fetch")
declare function fetch_(url: usize, urlLen: u32, body: usize, bodyLen: u32): i32;
@external("schemify", "clipboard_set")
declare function clipboard_set(ptr: usize, len: u32): i32;

const scratch = new Out(memory.data(SCRATCH_MAX), SCRATCH_MAX);
const argA = new Str();
const argB = new Str();

// Points dst at v's bytes: a Str as is, a string encoded into scratch.
function arg<T>(v: T, dst: Str): bool {
  if (!isString<T>()) {
    const s = changetype<Str>(v);
    dst.set(s.ptr, s.len);
    return true;
  }
  const start = scratch.len;
  scratch.put(v);
  dst.set(scratch.ptr + start, scratch.len - start);
  return !scratch.overflow;
}

function args1<A>(a: A): bool {
  scratch.clear();
  return arg(a, argA);
}

function args2<A, B>(a: A, b: B): bool {
  scratch.clear();
  return arg(a, argA) && arg(b, argB);
}

// The import wrappers take a string or a Str for each text or data argument; a string
// over SCRATCH_MAX bytes of UTF-8 answers -Code.Space without calling the host.

/** Evaluates a scalar expression in `doc`'s cell scope. Inverted: returns a handle; the
 *  value arrives as an io_done event in the same delivery. */
export function evaluate<T>(doc: u32, expr: T): i32 {
  return args1(expr) ? eval_(doc, argA.ptr, argA.len) : -Code.Space;
}

/** Logs at once, truncated to 4 KiB; kept only if the event completes. */
export function hostLog<T>(level: Level, text: T): void {
  if (args1(text)) log_(level, argA.ptr, argA.len);
}

/** Reads a project file (grant `read_project` or `read:<NAME>`); handle for io_done. */
export function fsRead<T>(path: T): i32 {
  return args1(path) ? fs_read(argA.ptr, argA.len) : -Code.Space;
}

/** Lists the files under a directory, one relative path per line of io_done's bytes. */
export function fsList<T>(path: T): i32 {
  return args1(path) ? fs_list(argA.ptr, argA.len) : -Code.Space;
}

/** Writes a project file (grant `write_project`); handle for io_done. */
export function fsWrite<P, D>(path: P, data: D): i32 {
  return args2(path, data) ? fs_write(argA.ptr, argA.len, argB.ptr, argB.len) : -Code.Space;
}

/** Removes a file or tree strictly inside `.schemify/` (grant `write_project`). */
export function fsRemove<T>(path: T): i32 {
  return args1(path) ? fs_remove(argA.ptr, argA.len) : -Code.Space;
}

/** Runs a granted binary (`spawn:<binary>`): `argv` is one line of line-record values
 *  (build it with Out.putValue); io_done carries the exit status and stdout then stderr. */
export function spawn<A, S>(argv: A, stdin: S): i32 {
  return args2(argv, stdin) ? spawn_(argA.ptr, argA.len, argB.ptr, argB.len) : -Code.Space;
}

/** Fetches `url` (grant `network`), sending `body`; handle for io_done. */
export function fetch<U, B>(url: U, body: B): i32 {
  return args2(url, body) ? fetch_(argA.ptr, argA.len, argB.ptr, argB.len) : -Code.Space;
}

/** Sets the clipboard's text (grant `clipboard`); handle for io_done. */
export function clipboardSet<T>(text: T): i32 {
  return args1(text) ? clipboard_set(argA.ptr, argA.len) : -Code.Space;
}

// ---- The exports (ABI §2) ----

/** Handles one event, writing its outputs to `out`. Every import works here. */
export type EventHandler = (ev: Event, out: Out) => void;
/** Pure expression function `fnId` (D20): `args` are line-record values; write one
 *  canonical value to `result` and return 0, or return a Code. Every import is denied. */
export type CallHandler = (fnId: u32, args: Str, result: Out) => u32;

function noEvent(ev: Event, out: Out): void {}
function noCall(fnId: u32, args: Str, result: Out): u32 {
  return Code.Range;
}

let eventHandler: EventHandler = noEvent;
let callHandler: CallHandler = noCall;

/** Sets the event handler; call it at top level (the start function). */
export function onEvent(h: EventHandler): void {
  eventHandler = h;
}

/** Sets the expression-function handler; the entry file must also re-export schemify_call. */
export function onCall(h: CallHandler): void {
  callHandler = h;
}

const IN = memory.data(INPUT_MAX);
const output = new Out(memory.data(OUTPUT_MAX), OUTPUT_MAX);
const event = new Event();
const callArgs = new Str();
let inLen: u32 = 0;

function pack(ptr: usize, len: u32): u64 {
  return (<u64>ptr << 32) | len;
}

export function schemify_abi(): u32 {
  return ABI_VERSION;
}

export function schemify_alloc(len: u32): usize {
  if (len > INPUT_MAX) return 0; // the host reports E_SPACE
  inLen = len;
  return IN;
}

export function schemify_on_event(ptr: usize, len: u32): u64 {
  if (ptr != IN || len != inLen) return 0;
  output.clear();
  if (!parseEvent(event, IN, len)) output.log(Level.Error, "unparsable event");
  else eventHandler(event, output);
  if (output.overflow) output.clear().log(Level.Error, "output over OUTPUT_MAX, dropped");
  return output.len ? pack(output.ptr, output.len) : 0;
}

export function schemify_call(fnId: u32, ptr: usize, len: u32): u64 {
  if (ptr != IN || len != inLen) return Code.Range;
  output.clear();
  let code = callHandler(fnId, callArgs.set(IN, len), output);
  if (code == 0 && (output.overflow || output.len == 0)) code = output.overflow ? Code.Space : Code.Parse;
  return code ? <u64>code : pack(output.ptr, output.len);
}
