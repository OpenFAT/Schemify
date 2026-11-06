/* The C library's conformance guest (../../CONFORMANCE.md). */
#define SCHEMIFY_FUNCTIONS
#define SCHEMIFY_IMPLEMENTATION
#include "schemify.h"

static char text_buf[1 << 16];

static void echo_args(sch_out *t, sch_str args) {
    char v[4096];
    int32_t n;
    while ((n = sch_values_next(&args, v, sizeof v)) >= 0) {
        sch_str s = {v, (uint32_t)n};
        sch_put(t, SCH_STR(" ["));
        sch_put(t, s);
        sch_put(t, SCH_STR("]"));
    }
    if (n != -1) sch_put(t, SCH_STR(" !parse"));
}

static void out_records(sch_out *out) {
    sch_contribute_scoped(out, SCH_KIND_ANNOTATIONS, SCH_STR("erc pass"),
                          SCH_STR("mark R1 warn \"hi\""));
    sch_edit(out, 1, 1, SCH_STR("size it"), SCH_STR("set_attr M1 w 2u"));
    sch_edit(out, 1, 0, SCH_STR("move"), SCH_STR("set_attr R1 at 10 20\nset_attr R2 at 0 0"));
    sch_run(out, SCH_STR("lib:tb"), SCH_STR("default"), SCH_STR("tt"));
    sch_vary(out, SCH_STR("vdd"), SCH_STR("1.8"));
    sch_vary(out, SCH_STR("w"), SCH_STR("two words"));
    sch_timer(out, 7, 250);
    sch_log(out, SCH_WARN, SCH_STR("q\"\\\tx\ny\x01"));
    sch_log(out, SCH_INFO, SCH_STR("done"));
}

static void read_nets(sch_out *out, sch_out *t) {
    static uint32_t pins[64];
    static char names[4096];
    int32_t n = sch_table_len(SCH_NETS);
    if (n < 0 || n > 64) n = 0;
    sch_read_column(SCH_NETS, SCH_NETS_PIN_COUNT, 0, n, pins, n * 4);
    int32_t got = sch_read_strings(SCH_NETS, SCH_NETS_NAME, 0, n, names, sizeof names);
    uint32_t at = 0;
    sch_str name;
    sch_put(t, SCH_STR("nets "));
    sch_put_u32(t, (uint32_t)n);
    sch_put(t, SCH_STR(":"));
    for (int32_t i = 0; i < n && got >= 0 && sch_strings_next(names, got, &at, &name); i++) {
        sch_put(t, SCH_STR(" "));
        sch_put(t, name);
        sch_put(t, SCH_STR("="));
        sch_put_u32(t, pins[i]);
    }
    sch_log(out, SCH_INFO, sch_out_str(t));
    t->len = 0;
    sch_put(t, SCH_STR("errors "));
    sch_put_i32(t, -sch_table_len(99));
    sch_put(t, SCH_STR(" "));
    sch_put_i32(t, -sch_read_column(SCH_NETS, SCH_NETS_NAME, 0, n, pins, n * 4));
    sch_put(t, SCH_STR(" "));
    sch_put_i32(t, -sch_read_column(SCH_NETS, SCH_NETS_PIN_COUNT, 0, n + 1, pins, (n + 1) * 4));
}

void sch_on_event(const sch_event *ev, sch_out *out) {
    sch_out t = SCH_OUT(text_buf);
    switch (ev->kind) {
    case SCH_EV_LOAD:
        sch_contribute(out, SCH_KIND_COMMANDS,
                       SCH_STR("command conf.echo \"Echo\"\ncommand conf.out \"Out\"\n"
                               "command conf.read \"Read\""));
        sch_contribute(out, SCH_KIND_FUNCTIONS,
                       SCH_STR("function conf.add 0 2\nfunction conf.greet 1 1"));
        sch_put(&t, SCH_STR("load"));
        break;
    case SCH_EV_UNLOAD: sch_put(&t, SCH_STR("unload")); break;
    case SCH_EV_SELECTION_CHANGED: sch_put(&t, SCH_STR("selection_changed")); break;
    case SCH_EV_COMMAND:
        if (sch_str_eq(ev->name, SCH_STR("conf.out"))) return out_records(out);
        if (sch_str_eq(ev->name, SCH_STR("conf.read"))) read_nets(out, &t);
        else {
            sch_put(&t, SCH_STR("command "));
            sch_put(&t, ev->name);
            echo_args(&t, ev->args);
        }
        break;
    case SCH_EV_DOC_OPENED: sch_put(&t, SCH_STR("doc_opened ")); sch_put_u32(&t, ev->doc); break;
    case SCH_EV_DOC_CLOSED: sch_put(&t, SCH_STR("doc_closed ")); sch_put_u32(&t, ev->doc); break;
    case SCH_EV_SAVED: sch_put(&t, SCH_STR("saved ")); sch_put_u32(&t, ev->doc); break;
    case SCH_EV_DOC_CHANGED:
        sch_put(&t, SCH_STR("doc_changed "));
        sch_put_u32(&t, ev->doc);
        sch_put(&t, SCH_STR(" "));
        sch_put_u32(&t, ev->epoch);
        break;
    case SCH_EV_NETLIST_REQUEST:
        sch_put(&t, SCH_STR("netlist_request "));
        sch_put(&t, ev->dialect);
        sch_put(&t, SCH_STR(" "));
        sch_put(&t, ev->bench);
        break;
    case SCH_EV_RUN_JOB:
        sch_put(&t, SCH_STR("run_job "));
        sch_put_u32(&t, ev->job);
        sch_put(&t, SCH_STR(" "));
        sch_put(&t, ev->lang);
        break;
    case SCH_EV_RUN_DONE: sch_put(&t, SCH_STR("run_done ")); sch_put_u32(&t, ev->job); break;
    case SCH_EV_TIMER: sch_put(&t, SCH_STR("timer ")); sch_put_u32(&t, ev->id); break;
    case SCH_EV_IO_DONE:
        sch_put(&t, SCH_STR("io_done "));
        sch_put_u32(&t, ev->handle);
        sch_put(&t, SCH_STR(" "));
        sch_put_i32(&t, ev->status);
        sch_put(&t, SCH_STR(" "));
        sch_put(&t, ev->bytes);
        break;
    case SCH_EV_REJECTED:
        sch_put(&t, SCH_STR("rejected "));
        sch_put_u32(&t, ev->code);
        sch_put(&t, SCH_STR(" "));
        sch_put(&t, ev->bytes);
        break;
    }
    sch_log(out, SCH_INFO, sch_out_str(&t));
}

/* Parses one decimal integer value; 0 on anything else. */
static int parse_int(sch_str *args, int64_t *v) {
    char b[24];
    int32_t n = sch_values_next(args, b, sizeof b);
    int64_t x = 0;
    int i = n > 0 && b[0] == '-';
    if (n <= i) return 0;
    for (; i < n; i++) {
        if (b[i] < '0' || b[i] > '9' || x > 1000000000000LL) return 0;
        x = x * 10 + (b[i] - '0');
    }
    *v = b[0] == '-' ? -x : x;
    return 1;
}

uint32_t sch_call(uint32_t fn_id, sch_str args, sch_out *result) {
    if (fn_id == 0) {
        int64_t a, b;
        char extra[1];
        if (!parse_int(&args, &a) || !parse_int(&args, &b) ||
            sch_values_next(&args, extra, sizeof extra) != -1)
            return SCH_E_PARSE;
        int64_t sum = a + b;
        if (sum < 0) sch_put(result, SCH_STR("-"));
        char d[24];
        uint32_t k = sizeof d;
        uint64_t u = sum < 0 ? (uint64_t)-sum : (uint64_t)sum;
        do d[--k] = (char)('0' + u % 10);
        while (u /= 10);
        sch_str s = {d + k, (uint32_t)sizeof d - k};
        sch_put(result, s);
        return 0;
    }
    if (fn_id == 1) {
        char v[1024], g[1100];
        int32_t n = sch_values_next(&args, v, sizeof v);
        if (n < 0) return SCH_E_PARSE;
        sch_out greet = SCH_OUT(g);
        sch_put(&greet, SCH_STR("hi "));
        sch_str s = {v, (uint32_t)n};
        sch_put(&greet, s);
        sch_put_value(result, sch_out_str(&greet));
        return 0;
    }
    return SCH_E_RANGE;
}
