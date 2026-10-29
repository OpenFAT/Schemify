// A Schemify plugin in D: Tools → Count nets logs the nets with a single connection.
// schemify.c compiles Schemify's C library (the exports); `import schemify` reads the same
// file through ImportC for its types, constants and helpers. -betterC: no druntime, no GC.
module hello;

import ldc.attributes : llvmAttr;
import schemify;

// ImportC ignores schemify.h's import attributes, so each host import you call is declared
// here once more with its module; this declaration shadows the imported one.
extern (C) {
    @llvmAttr("wasm-import-module", "schemify") @llvmAttr("wasm-import-name", "table_len")
    int sch_table_len(uint table);
    @llvmAttr("wasm-import-module", "schemify") @llvmAttr("wasm-import-name", "read_column")
    int sch_read_column(uint table, uint column, uint start, uint count, void* dst, uint dstLen);
    @llvmAttr("wasm-import-module", "schemify") @llvmAttr("wasm-import-name", "read_strings")
    int sch_read_strings(uint table, uint column, uint start, uint count, void* dst, uint dstLen);
}

sch_str str(const(char)[] s) => sch_str(s.ptr, cast(uint) s.length);

__gshared uint[1 << 16] pins;
__gshared char[1 << 20] names = void; // `= void`: char.init is 0xFF, 1 MiB of data otherwise

void countNets(sch_out* o) {
    int n = sch_table_len(SCH_NETS);
    if (n < 0 || n > pins.length) n = 0;
    if (sch_read_column(SCH_NETS, SCH_NETS_PIN_COUNT, 0, n, pins.ptr, n * 4) < 0) n = 0;
    int got = sch_read_strings(SCH_NETS, SCH_NETS_NAME, 0, n, names.ptr, names.length);
    if (got < 0) got = 0;

    char[1024] listBuf = void;
    char[1100] msgBuf = void;
    auto list = sch_out(listBuf.ptr, 0, listBuf.length, 0);
    auto msg = sch_out(msgBuf.ptr, 0, msgBuf.length, 0);
    uint single = 0, at = 0;
    sch_str name;
    for (int i = 0; i < n && sch_strings_next(names.ptr, got, &at, &name); i++) {
        if (pins[i] != 1) continue;
        sch_put(&list, str(single++ ? ", " : ": "));
        sch_put(&list, name);
    }
    sch_put_u32(&msg, single);
    sch_put(&msg, str(" single-connection nets"));
    sch_put(&msg, sch_out_str(&list));
    sch_log(o, SCH_INFO, sch_out_str(&msg));
}

extern (C) void sch_on_event(const(sch_event)* ev, sch_out* o) {
    switch (ev.kind) {
    case SCH_EV_LOAD:
    case SCH_EV_DOC_CHANGED:
        // Contributions are immediate-mode: send the whole set every time.
        sch_contribute(o, SCH_KIND_UI.ptr, str("node tools.hello item\n" ~
                                           "  parent menu.tools\n" ~
                                           "  label \"Count nets\"\n" ~
                                           "  command hello.count\n"));
        sch_contribute(o, SCH_KIND_COMMANDS.ptr, str("command hello.count \"Count nets\"\n"));
        break;
    case SCH_EV_COMMAND:
        if (sch_str_eq(ev.name, str("hello.count"))) countNets(o);
        break;
    default:
        break;
    }
}
