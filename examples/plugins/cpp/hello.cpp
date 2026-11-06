// A Schemify plugin in C++: Tools → Count nets logs the nets with a single connection.
// Freestanding: no exceptions, RTTI or standard library (they need a libc).
#define SCHEMIFY_IMPLEMENTATION
#include "schemify.h"

namespace {

uint32_t pins[1 << 16];
char names[1 << 20];

constexpr sch_str ui = SCH_STR("node tools.hello item\n"
                                "  parent menu.tools\n"
                                "  label \"Count nets\"\n"
                                "  command hello.count\n");
constexpr sch_str commands = SCH_STR("command hello.count \"Count nets\"\n");

// A sch_out over its own fixed buffer, for composing text.
template <uint32_t N> struct Text {
    char buf[N];
    sch_out out = {buf, 0, N, 0};
    Text &operator<<(sch_str s) { return sch_put(&out, s), *this; }
    Text &operator<<(uint32_t v) { return sch_put_u32(&out, v), *this; }
    sch_str str() const { return sch_out_str(&out); }
};

void count_nets(sch_out *out) {
    int32_t n = sch_table_len(SCH_NETS);
    if (n < 0 || n > int32_t(sizeof pins / sizeof *pins)) n = 0;
    if (sch_read_column(SCH_NETS, SCH_NETS_PIN_COUNT, 0, n, pins, n * 4) < 0) n = 0;
    int32_t got = sch_read_strings(SCH_NETS, SCH_NETS_NAME, 0, n, names, sizeof names);
    if (got < 0) got = 0;

    Text<1024> list;
    uint32_t single = 0, at = 0;
    sch_str name;
    for (int32_t i = 0; i < n && sch_strings_next(names, got, &at, &name); i++)
        if (pins[i] == 1) list << (single++ ? SCH_STR(", ") : SCH_STR(": ")) << name;
    Text<1100> msg;
    msg << single << SCH_STR(" single-connection nets") << list.str();
    sch_log(out, SCH_INFO, msg.str());
}

} // namespace

void sch_on_event(const sch_event *ev, sch_out *out) {
    switch (ev->kind) {
    case SCH_EV_LOAD:
    case SCH_EV_DOC_CHANGED:
        // Contributions are immediate-mode: send the whole set every time.
        sch_contribute(out, SCH_KIND_UI, ui);
        sch_contribute(out, SCH_KIND_COMMANDS, commands);
        break;
    case SCH_EV_COMMAND:
        if (sch_str_eq(ev->name, SCH_STR("hello.count"))) count_nets(out);
        break;
    default:
        break;
    }
}
