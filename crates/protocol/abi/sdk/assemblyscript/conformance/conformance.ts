// The AssemblyScript library's conformance guest (../../CONFORMANCE.md).
import {
  Code, Event, EventKind, KIND_ANNOTATIONS, KIND_COMMANDS, KIND_FUNCTIONS, Level, NETS_NAME,
  NETS_PIN_COUNT, Out, Str, Table, onCall, onEvent, readColumn, readStrings, tableLen,
} from "../schemify";
export { schemify_abi, schemify_alloc, schemify_call, schemify_on_event } from "../schemify";

const text = new Out(memory.data(1 << 16), 1 << 16);
const value = new Out(memory.data(4096), 4096);
const PINS = memory.data(64 * 4);
const NAMES = memory.data(4096);
const names = new Str();
const name = new Str();

function echoArgs(t: Out, args: Str): void {
  let n: i32;
  while ((n = args.nextValue(value)) >= 0) t.put(" [").put(value).put("]");
  if (n != -1) t.put(" !parse");
}

function outRecords(out: Out): void {
  out.contributeScoped(KIND_ANNOTATIONS, "erc pass", 'mark R1 warn "hi"');
  out.stage(1, "size it", "set_attr M1 w 2u");
  out.commit(1, "move", "set_attr R1 at 10 20\nset_attr R2 at 0 0");
  out.run("lib:tb", "default", "tt").vary("vdd", "1.8").vary("w", "two words");
  out.timer(7, 250);
  out.log(Level.Warn, 'q"\\\tx\ny\x01');
  out.log(Level.Info, "done");
}

function readNets(out: Out, t: Out): void {
  let n = tableLen(Table.Nets);
  if (n < 0 || n > 64) n = 0;
  readColumn(Table.Nets, NETS_PIN_COUNT, 0, n, PINS, n * 4);
  const got = readStrings(Table.Nets, NETS_NAME, 0, n, NAMES, 4096);
  names.set(NAMES, got < 0 ? 0 : got);
  t.put("nets ").put(n).put(":");
  for (let i = 0; i < n && names.nextString(name); i++) t.put(" ").put(name).put("=").put(load<u32>(PINS + i * 4));
  out.log(Level.Info, t);
  t.clear().put("errors ").put(-tableLen(99));
  t.put(" ").put(-readColumn(Table.Nets, NETS_NAME, 0, n, PINS, n * 4));
  t.put(" ").put(-readColumn(Table.Nets, NETS_PIN_COUNT, 0, n + 1, PINS, (n + 1) * 4));
}

onEvent((ev: Event, out: Out): void => {
  const t = text.clear();
  switch (ev.kind) {
    case EventKind.Load:
      out.contribute(KIND_COMMANDS, 'command conf.echo "Echo"\ncommand conf.out "Out"\ncommand conf.read "Read"');
      out.contribute(KIND_FUNCTIONS, "function conf.add 0 2\nfunction conf.greet 1 1");
      t.put("load");
      break;
    case EventKind.Unload: t.put("unload"); break;
    case EventKind.SelectionChanged: t.put("selection_changed"); break;
    case EventKind.Command:
      if (ev.name.eq("conf.out")) return outRecords(out);
      if (ev.name.eq("conf.read")) readNets(out, t);
      else {
        t.put("command ").put(ev.name);
        echoArgs(t, ev.args);
      }
      break;
    case EventKind.DocOpened: t.put("doc_opened ").put(ev.doc); break;
    case EventKind.DocClosed: t.put("doc_closed ").put(ev.doc); break;
    case EventKind.Saved: t.put("saved ").put(ev.doc); break;
    case EventKind.DocChanged: t.put("doc_changed ").put(ev.doc).put(" ").put(ev.epoch); break;
    case EventKind.NetlistRequest: t.put("netlist_request ").put(ev.dialect).put(" ").put(ev.bench); break;
    case EventKind.RunJob: t.put("run_job ").put(ev.job).put(" ").put(ev.lang); break;
    case EventKind.RunDone: t.put("run_done ").put(ev.job); break;
    case EventKind.Timer: t.put("timer ").put(ev.id); break;
    case EventKind.IoDone: t.put("io_done ").put(ev.handle).put(" ").put(ev.status).put(" ").put(ev.bytes); break;
    case EventKind.Rejected: t.put("rejected ").put(ev.code).put(" ").put(ev.bytes); break;
  }
  out.log(Level.Info, t);
});

// One decimal integer value into `int`; false on anything else.
let int: i64 = 0;
function readInt(args: Str): bool {
  const n = args.nextValue(value);
  const neg = n > 0 && value.byte(0) == 0x2d;
  let i = neg ? 1 : 0;
  if (n <= i) return false;
  let x: i64 = 0;
  for (; i < n; i++) {
    const c = value.byte(i);
    if (c < 0x30 || c > 0x39 || x > 1000000000000) return false;
    x = x * 10 + (c - 0x30);
  }
  int = neg ? -x : x;
  return true;
}

onCall((fnId: u32, args: Str, result: Out): u32 => {
  if (fnId == 0) {
    if (!readInt(args)) return Code.Parse;
    const a = int;
    if (!readInt(args) || args.nextValue(value) != -1) return Code.Parse;
    result.put(a + int);
    return 0;
  }
  if (fnId == 1) {
    if (args.nextValue(value) < 0) return Code.Parse;
    text.clear().put("hi ").put(value);
    result.putValue(text);
    return 0;
  }
  return Code.Range;
});
