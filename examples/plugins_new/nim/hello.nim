# A Schemify plugin in Nim: Tools → Count nets logs the nets with a single connection.
# Schemify's C library (schemify.c) owns the exports and calls sch_on_event below.
{.compile: "schemify.c".}

const h = "schemify.h"
type
  SchStr {.importc: "sch_str", header: h, bycopy.} = object
    p {.importc: "ptr".}: ptr UncheckedArray[char]
    len: uint32
  SchOut {.importc: "sch_out", header: h, bycopy.} = object
    buf: ptr char
    len, cap: uint32
    overflow: cint
  # "const" so sch_on_event's definition matches schemify.h's prototype.
  SchEvent {.importc: "const sch_event", header: h.} = object
    kind: cint
    name: SchStr

var
  SCH_NETS {.importc, header: h.}: uint32
  SCH_NETS_NAME {.importc, header: h.}: uint32
  SCH_NETS_PIN_COUNT {.importc, header: h.}: uint32
  SCH_INFO {.importc, header: h.}: cint
  SCH_EV_LOAD {.importc, header: h.}: cint
  SCH_EV_DOC_CHANGED {.importc, header: h.}: cint
  SCH_EV_COMMAND {.importc, header: h.}: cint
  SCH_KIND_UI {.importc, header: h.}: cstring
  SCH_KIND_COMMANDS {.importc, header: h.}: cstring

{.push importc, header: h.}
proc sch_table_len(table: uint32): int32
proc sch_read_column(table, column, start, count: uint32, dst: pointer, dstLen: uint32): int32
proc sch_read_strings(table, column, start, count: uint32, dst: pointer, dstLen: uint32): int32
proc sch_strings_next(buf: pointer, bufLen: uint32, at: ptr uint32, s: ptr SchStr): cint
proc sch_str_eq(a, b: SchStr): cint
proc sch_out_str(o: ptr SchOut): SchStr
proc sch_put(o: ptr SchOut, s: SchStr)
proc sch_put_u32(o: ptr SchOut, v: uint32)
proc sch_log(o: ptr SchOut, level: cint, text: SchStr)
proc sch_contribute(o: ptr SchOut, kind: cstring, body: SchStr)
{.pop.}

proc str(s: static string): SchStr =
  SchStr(p: cast[ptr UncheckedArray[char]](cstring(s)), len: s.len.uint32)

proc outOn(buf: var openArray[char]): SchOut =
  SchOut(buf: buf[0].addr, cap: buf.len.uint32)

var pins: array[1 shl 16, uint32]
var names: array[1 shl 20, char]

proc countNets(o: ptr SchOut) =
  var n = sch_table_len(SCH_NETS)
  if n < 0 or n > pins.len.int32: n = 0
  if sch_read_column(SCH_NETS, SCH_NETS_PIN_COUNT, 0, n.uint32, pins.addr, n.uint32 * 4) < 0: n = 0
  var got = sch_read_strings(SCH_NETS, SCH_NETS_NAME, 0, n.uint32, names.addr, names.len.uint32)
  if got < 0: got = 0

  var listBuf: array[1024, char]
  var msgBuf: array[1100, char]
  var list = outOn(listBuf)
  var msg = outOn(msgBuf)
  var single, at: uint32
  var name: SchStr
  for i in 0 ..< n:
    if sch_strings_next(names.addr, got.uint32, at.addr, name.addr) == 0: break
    if pins[i] != 1: continue
    sch_put(list.addr, if single == 0: str(": ") else: str(", "))
    sch_put(list.addr, name)
    inc single
  sch_put_u32(msg.addr, single)
  sch_put(msg.addr, str(" single-connection nets"))
  sch_put(msg.addr, sch_out_str(list.addr))
  sch_log(o, SCH_INFO, sch_out_str(msg.addr))

proc onEvent(ev: ptr SchEvent, o: ptr SchOut) {.exportc: "sch_on_event".} =
  if ev.kind == SCH_EV_LOAD or ev.kind == SCH_EV_DOC_CHANGED:
    # Contributions are immediate-mode: send the whole set every time.
    sch_contribute(o, SCH_KIND_UI, str("node tools.hello item\n" &
                                       "  parent menu.tools\n" &
                                       "  label \"Count nets\"\n" &
                                       "  command hello.count\n"))
    sch_contribute(o, SCH_KIND_COMMANDS, str("command hello.count \"Count nets\"\n"))
  elif ev.kind == SCH_EV_COMMAND and sch_str_eq(ev.name, str("hello.count")) != 0:
    countNets(o)
