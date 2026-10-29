// A Schemify plugin in Embedded Swift: Tools → Count nets logs the nets with a single connection.
// Schemify's C library (Sources/CSchemify) owns the exports and calls sch_on_event below.
import CSchemify

/// A `sch_str` over a string literal (static storage, so the pointer outlives the call).
func str(_ s: StaticString) -> sch_str {
    sch_str(ptr: UnsafeRawPointer(s.utf8Start).assumingMemoryBound(to: CChar.self),
            len: UInt32(s.utf8CodeUnitCount))
}

nonisolated(unsafe) var pins = InlineArray<65536, UInt32>(repeating: 0)
nonisolated(unsafe) var names = InlineArray<1048576, CChar>(repeating: 0)
nonisolated(unsafe) var listBuf = InlineArray<1024, CChar>(repeating: 0)
nonisolated(unsafe) var msgBuf = InlineArray<1100, CChar>(repeating: 0)

func countNets(_ out: UnsafeMutablePointer<sch_out>) {
    var n = sch_table_len(UInt32(SCH_NETS.rawValue))
    if n < 0 || n > pins.count { n = 0 }
    if sch_read_column(UInt32(SCH_NETS.rawValue), UInt32(SCH_NETS_PIN_COUNT.rawValue), 0, UInt32(n),
                       &pins, UInt32(n) * 4) < 0 { n = 0 }
    var got = sch_read_strings(UInt32(SCH_NETS.rawValue), UInt32(SCH_NETS_NAME.rawValue), 0, UInt32(n),
                               &names, UInt32(names.count))
    if got < 0 { got = 0 }

    // sch_out keeps a pointer, so it must be one that stays valid for the whole scope.
    withUnsafeMutableBytes(of: &listBuf) { listBuf in
        withUnsafeMutableBytes(of: &msgBuf) { msgBuf in
            var list = sch_out(buf: listBuf.baseAddress!.assumingMemoryBound(to: CChar.self), len: 0,
                               cap: UInt32(listBuf.count), overflow: 0)
            var msg = sch_out(buf: msgBuf.baseAddress!.assumingMemoryBound(to: CChar.self), len: 0,
                              cap: UInt32(msgBuf.count), overflow: 0)
            var single: UInt32 = 0, at: UInt32 = 0
            var name = sch_str()
            for i in 0..<Int(n) {
                if sch_strings_next(&names, UInt32(got), &at, &name) == 0 { break }
                if pins[i] != 1 { continue }
                sch_put(&list, single == 0 ? str(": ") : str(", "))
                sch_put(&list, name)
                single += 1
            }
            sch_put_u32(&msg, single)
            sch_put(&msg, str(" single-connection nets"))
            sch_put(&msg, sch_out_str(&list))
            sch_log(out, SCH_INFO, sch_out_str(&msg))
        }
    }
}

// The C name and calling convention schemify.h declares; the library calls it per event.
@_cdecl("sch_on_event")
func onEvent(_ ev: UnsafePointer<sch_event>, _ out: UnsafeMutablePointer<sch_out>) {
    switch ev.pointee.kind {
    case SCH_EV_LOAD, SCH_EV_DOC_CHANGED:
        // Contributions are immediate-mode: send the whole set every time.
        sch_contribute(out, SCH_KIND_UI, str("""
            node tools.hello item
              parent menu.tools
              label "Count nets"
              command hello.count

            """))
        sch_contribute(out, SCH_KIND_COMMANDS, str("command hello.count \"Count nets\"\n"))
    case SCH_EV_COMMAND:
        if sch_str_eq(ev.pointee.name, str("hello.count")) != 0 { countNets(out) }
    default:
        break
    }
}
