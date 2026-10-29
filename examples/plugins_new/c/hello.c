/* A Schemify plugin in C: Tools → Count nets logs the nets with a single connection. */
#define SCHEMIFY_IMPLEMENTATION
#include "schemify.h"

static uint32_t pins[1 << 16];
static char names[1 << 20];

static void count_nets(sch_out *out) {
    int32_t n = sch_table_len(SCH_NETS);
    if (n < 0 || n > (int32_t)(sizeof pins / sizeof *pins)) n = 0;
    if (sch_read_column(SCH_NETS, SCH_NETS_PIN_COUNT, 0, n, pins, n * 4) < 0) n = 0;
    int32_t got = sch_read_strings(SCH_NETS, SCH_NETS_NAME, 0, n, names, sizeof names);
    if (got < 0) got = 0;

    char list_buf[1024], msg_buf[1100];
    sch_out list = SCH_OUT(list_buf), msg = SCH_OUT(msg_buf);
    uint32_t single = 0, at = 0;
    sch_str name;
    for (int32_t i = 0; i < n && sch_strings_next(names, got, &at, &name); i++) {
        if (pins[i] != 1) continue;
        sch_put(&list, single++ ? SCH_STR(", ") : SCH_STR(": "));
        sch_put(&list, name);
    }
    sch_put_u32(&msg, single);
    sch_put(&msg, SCH_STR(" single-connection nets"));
    sch_put(&msg, sch_out_str(&list));
    sch_log(out, SCH_INFO, sch_out_str(&msg));
}

void sch_on_event(const sch_event *ev, sch_out *out) {
    switch (ev->kind) {
    case SCH_EV_LOAD:
    case SCH_EV_DOC_CHANGED:
        /* Contributions are immediate-mode: send the whole set every time. */
        sch_contribute(out, SCH_KIND_UI, SCH_STR("node tools.hello item\n"
                                                 "  parent menu.tools\n"
                                                 "  label \"Count nets\"\n"
                                                 "  command hello.count\n"));
        sch_contribute(out, SCH_KIND_COMMANDS, SCH_STR("command hello.count \"Count nets\"\n"));
        break;
    case SCH_EV_COMMAND:
        if (sch_str_eq(ev->name, SCH_STR("hello.count"))) count_nets(out);
        break;
    default:
        break;
    }
}
