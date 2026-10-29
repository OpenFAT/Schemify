// A Schemify plugin in AssemblyScript: Tools → Count nets logs the nets with a single connection.
// Build: npm install && npm run build (asconfig.json: stub runtime, no env.abort import).
import {
  Event, EventKind, KIND_COMMANDS, KIND_UI, Level, NETS_NAME, NETS_PIN_COUNT, Out, Str, Table,
  onEvent, readColumn, readStrings, tableLen,
} from "@schemify/sdk/schemify";
// Only the entry file's exports reach the module, so re-export the library's.
export { schemify_abi, schemify_alloc, schemify_on_event } from "@schemify/sdk/schemify";

// Static buffers: the stub runtime never frees, so nothing is allocated per event.
const PINS_MAX = 1 << 16;
const PINS = memory.data(PINS_MAX * 4);
const NAMES_MAX = 1 << 20;
const NAMES = memory.data(NAMES_MAX);
const names = new Str();
const name = new Str();
const list = new Out(memory.data(1024), 1024);
const msg = new Out(memory.data(1100), 1100);

function countNets(out: Out): void {
  let n = tableLen(Table.Nets);
  if (n < 0 || n > PINS_MAX) n = 0;
  if (readColumn(Table.Nets, NETS_PIN_COUNT, 0, n, PINS, n * 4) < 0) n = 0;
  const got = readStrings(Table.Nets, NETS_NAME, 0, n, NAMES, NAMES_MAX);
  names.set(NAMES, got < 0 ? 0 : got);
  list.clear();
  let single = 0;
  for (let i = 0; i < n && names.nextString(name); i++) {
    if (load<u32>(PINS + i * 4) != 1) continue;
    list.put(single++ ? ", " : ": ").put(name);
  }
  out.log(Level.Info, msg.clear().put(single).put(" single-connection nets").put(list));
}

onEvent((ev: Event, out: Out): void => {
  if (ev.kind == EventKind.Load || ev.kind == EventKind.DocChanged) {
    // Contributions are immediate-mode: send the whole set every time.
    out.contribute(KIND_UI, `node tools.hello item
  parent menu.tools
  label "Count nets"
  command hello.count`);
    out.contribute(KIND_COMMANDS, 'command hello.count "Count nets"');
  } else if (ev.kind == EventKind.Command && ev.name.eq("hello.count")) countNets(out);
});
