/* schemify.h: the Schemify plugin ABI v1 for C and C++ (docs/spec/plugin-abi.md).
 *
 * Single header, no libc. In exactly one source file:
 *
 *     #define SCHEMIFY_IMPLEMENTATION
 *     #include "schemify.h"
 *
 *     void sch_on_event(const sch_event *ev, sch_out *out) { ... }
 *
 * That file then provides the module's exports (schemify_abi, schemify_alloc,
 * schemify_on_event and, with SCHEMIFY_FUNCTIONS defined, schemify_call), and the plugin
 * writes only sch_on_event (and sch_call). Build for wasm32 with no libc and no entry
 * point: clang --target=wasm32 -nostdlib -Wl,--no-entry.
 *
 * Buffers are static: an event over SCHEMIFY_INPUT_MAX bytes is refused (the host reports
 * E_SPACE), and output past SCHEMIFY_OUTPUT_MAX is replaced by one error log. Define
 * either before the implementation to change it. */
#ifndef SCHEMIFY_H
#define SCHEMIFY_H

#include <stdint.h> /* the compiler's freestanding header */

#ifdef __cplusplus
extern "C" {
#endif

/* ---- ABI numbers, from crates/protocol/abi (cargo test -p schemify-abi --test sdk) ---- */
/* BEGIN GENERATED */
#define SCH_ABI_VERSION 1u

/* Error codes (ABI §8); imports return them negated. */
enum sch_code {
    SCH_E_DENIED = 1,
    SCH_E_GONE = 2,
    SCH_E_SPACE = 3,
    SCH_E_TABLE = 4,
    SCH_E_COLUMN = 5,
    SCH_E_RANGE = 6,
    SCH_E_PARSE = 7,
    SCH_E_FUEL = 8,
    SCH_E_VERSION = 9,
    SCH_E_TRAP = 10,
    SCH_E_MODULE = 11,
};
static const char *const sch_code_names[] = {
    0,
    "E_DENIED",
    "E_GONE",
    "E_SPACE",
    "E_TABLE",
    "E_COLUMN",
    "E_RANGE",
    "E_PARSE",
    "E_FUEL",
    "E_VERSION",
    "E_TRAP",
    "E_MODULE",
};

/* Snapshot tables (ABI §4). */
enum sch_table {
    SCH_DOCS = 0,
    SCH_INSTANCES = 1,
    SCH_WIRES = 2,
    SCH_LABELS = 3,
    SCH_NETS = 4,
    SCH_INST_PARAMS = 5,
    SCH_PATHS = 6,
    SCH_NAME_MAP = 7,
    SCH_VIOLATIONS = 8,
    SCH_DOC_VARS = 9,
    SCH_BENCH_SPECS = 10,
    SCH_BENCH_OUTPUTS = 11,
    SCH_RESULTS = 12,
    SCH_SELECTION = 13,
    SCH_UI_NODES = 14,
};

/* Column ids, SCH_<TABLE>_<COLUMN>, with their element types. */
enum sch_column {
    SCH_DOCS_HANDLE = 0, /* u32 */
    SCH_DOCS_KIND = 1, /* u8 */
    SCH_DOCS_DIRTY = 2, /* u8 */
    SCH_DOCS_PATH = 3, /* str */
    SCH_DOCS_CELL = 4, /* str */
    SCH_INSTANCES_DOC = 0, /* u32 */
    SCH_INSTANCES_ID = 1, /* u32 */
    SCH_INSTANCES_X = 2, /* i32 */
    SCH_INSTANCES_Y = 3, /* i32 */
    SCH_INSTANCES_ORIENT = 4, /* u8 */
    SCH_INSTANCES_CELL = 5, /* str */
    SCH_INSTANCES_NAME = 6, /* str */
    SCH_WIRES_DOC = 0, /* u32 */
    SCH_WIRES_ID = 1, /* u32 */
    SCH_WIRES_X1 = 2, /* i32 */
    SCH_WIRES_Y1 = 3, /* i32 */
    SCH_WIRES_X2 = 4, /* i32 */
    SCH_WIRES_Y2 = 5, /* i32 */
    SCH_WIRES_NET = 6, /* u32 */
    SCH_LABELS_DOC = 0, /* u32 */
    SCH_LABELS_ID = 1, /* u32 */
    SCH_LABELS_X = 2, /* i32 */
    SCH_LABELS_Y = 3, /* i32 */
    SCH_LABELS_NET = 4, /* u32 */
    SCH_LABELS_TEXT = 5, /* str */
    SCH_NETS_NAME = 0, /* str */
    SCH_NETS_WIDTH = 1, /* u16 */
    SCH_NETS_GLOBAL = 2, /* u8 */
    SCH_NETS_PIN_COUNT = 3, /* u32 */
    SCH_INST_PARAMS_INST_ROW = 0, /* u32 */
    SCH_INST_PARAMS_NAME = 1, /* str */
    SCH_INST_PARAMS_VALUE = 2, /* str */
    SCH_INST_PARAMS_SOURCE = 3, /* u8 */
    SCH_PATHS_PATH = 0, /* str */
    SCH_PATHS_CELL = 1, /* str */
    SCH_PATHS_IMPL = 2, /* str */
    SCH_PATHS_PARENT = 3, /* u32 */
    SCH_NAME_MAP_PATH = 0, /* str */
    SCH_NAME_MAP_EMITTED = 1, /* str */
    SCH_VIOLATIONS_CODE = 0, /* str */
    SCH_VIOLATIONS_SEVERITY = 1, /* u8 */
    SCH_VIOLATIONS_OBJECT = 2, /* u32 */
    SCH_VIOLATIONS_PARAM = 3, /* str */
    SCH_DOC_VARS_NAME = 0, /* str */
    SCH_DOC_VARS_EXPR = 1, /* str */
    SCH_DOC_VARS_VALUE = 2, /* str */
    SCH_BENCH_SPECS_NAME = 0, /* str */
    SCH_BENCH_SPECS_EXPR = 1, /* str */
    SCH_BENCH_SPECS_MIN = 2, /* str */
    SCH_BENCH_SPECS_MAX = 3, /* str */
    SCH_BENCH_OUTPUTS_NAME = 0, /* str */
    SCH_BENCH_OUTPUTS_EXPR = 1, /* str */
    SCH_BENCH_OUTPUTS_MIN = 2, /* str */
    SCH_BENCH_OUTPUTS_MAX = 3, /* str */
    SCH_RESULTS_JOB = 0, /* u32 */
    SCH_RESULTS_OUTPUT = 1, /* str */
    SCH_RESULTS_VALUE = 2, /* str */
    SCH_RESULTS_PASS = 3, /* u8 */
    SCH_SELECTION_DOC = 0, /* u32 */
    SCH_SELECTION_ID = 1, /* u32 */
    SCH_UI_NODES_ID = 0, /* str */
    SCH_UI_NODES_KIND = 1, /* u8 */
    SCH_UI_NODES_PARENT = 2, /* str */
    SCH_UI_NODES_OWNER = 3, /* u16 */
};

/* Log levels. */
enum sch_level {
    SCH_ERROR = 0,
    SCH_WARN = 1,
    SCH_INFO = 2,
    SCH_DEBUG = 3,
};

/* Contribution kinds (ABI §1). */
#define SCH_KIND_UI "ui"
#define SCH_KIND_COMMANDS "commands"
#define SCH_KIND_KEYMAP "keymap"
#define SCH_KIND_ANNOTATIONS "annotations"
#define SCH_KIND_FUNCTIONS "functions"
#define SCH_KIND_DIALECTS "dialects"
#define SCH_KIND_RUNNERS "runners"
#define SCH_KIND_CHECKS "checks"
#define SCH_KIND_CONSTRAINT_TYPES "constraint_types"
/* END GENERATED */

/* ---- Strings: a borrowed byte range, never NUL-terminated ---- */

typedef struct sch_str {
    const char *ptr;
    uint32_t len;
} sch_str;

#ifdef __cplusplus
#define SCH_STR(lit) (sch_str{(lit), sizeof(lit) - 1})
#else
#define SCH_STR(lit) ((sch_str){(lit), sizeof(lit) - 1})
#endif

static inline sch_str sch_cstr(const char *s) {
    uint32_t n = 0;
    while (s[n]) n++;
    sch_str r = {s, n};
    return r;
}

static inline int sch_str_eq(sch_str a, sch_str b) {
    if (a.len != b.len) return 0;
    for (uint32_t i = 0; i < a.len; i++)
        if (a.ptr[i] != b.ptr[i]) return 0;
    return 1;
}

/* ---- Host imports (ABI §3, §5). Negative returns are -sch_code. ---- */

#define SCH_IMPORT(name) __attribute__((import_module("schemify"), import_name(name)))

SCH_IMPORT("table_len") int32_t sch_table_len(uint32_t table);
SCH_IMPORT("table_epoch") int32_t sch_table_epoch(uint32_t table);
/* Fixed-width column slice, little-endian, into dst; returns bytes written. */
SCH_IMPORT("read_column")
int32_t sch_read_column(uint32_t table, uint32_t column, uint32_t start, uint32_t count,
                        void *dst, uint32_t dst_len);
/* `count` strings as u32 length + UTF-8 bytes each; walk them with sch_strings_next. On
 * -SCH_E_SPACE the needed size is in the first 4 bytes of dst. */
SCH_IMPORT("read_strings")
int32_t sch_read_strings(uint32_t table, uint32_t column, uint32_t start, uint32_t count,
                         void *dst, uint32_t dst_len);
/* Inverted: returns a handle; the value arrives as an io_done event. */
SCH_IMPORT("eval") int32_t sch_eval(uint32_t doc, const char *expr, uint32_t len);
SCH_IMPORT("log") void sch_host_log(uint32_t level, const char *text, uint32_t len);
/* Capability imports (grant in schemify.project); each returns a handle for io_done. */
SCH_IMPORT("fs_read") int32_t sch_fs_read(const char *path, uint32_t path_len);
SCH_IMPORT("fs_list") int32_t sch_fs_list(const char *path, uint32_t path_len);
SCH_IMPORT("fs_write")
int32_t sch_fs_write(const char *path, uint32_t path_len, const void *data, uint32_t data_len);
SCH_IMPORT("fs_remove") int32_t sch_fs_remove(const char *path, uint32_t path_len);
SCH_IMPORT("spawn")
int32_t sch_spawn(const char *argv, uint32_t argv_len, const void *stdin_, uint32_t stdin_len);
SCH_IMPORT("fetch")
int32_t sch_fetch(const char *url, uint32_t url_len, const void *body, uint32_t body_len);
SCH_IMPORT("clipboard_get") int32_t sch_clipboard_get(void);
SCH_IMPORT("clipboard_set") int32_t sch_clipboard_set(const char *text, uint32_t len);

/* Next string of a sch_read_strings buffer; 0 when none is left. `*at` starts at 0. */
static inline int sch_strings_next(const void *buf, uint32_t buf_len, uint32_t *at, sch_str *s) {
    const unsigned char *b = (const unsigned char *)buf;
    if (*at + 4 > buf_len) return 0;
    uint32_t n = b[*at] | (uint32_t)b[*at + 1] << 8 | (uint32_t)b[*at + 2] << 16 |
                 (uint32_t)b[*at + 3] << 24;
    if (n > buf_len - *at - 4) return 0;
    s->ptr = (const char *)b + *at + 4;
    s->len = n;
    *at += 4 + n;
    return 1;
}

/* ---- Events (ABI §6) ---- */

typedef enum sch_event_kind {
    SCH_EV_LOAD,
    SCH_EV_UNLOAD,
    SCH_EV_COMMAND,
    SCH_EV_DOC_OPENED,
    SCH_EV_DOC_CLOSED,
    SCH_EV_DOC_CHANGED,
    SCH_EV_SELECTION_CHANGED,
    SCH_EV_NETLIST_REQUEST,
    SCH_EV_RUN_JOB,
    SCH_EV_RUN_DONE,
    SCH_EV_IO_DONE,
    SCH_EV_TIMER,
    SCH_EV_SAVED,
    SCH_EV_REJECTED,
} sch_event_kind;

/* One event; the fields its kind uses are set, the rest are zero. Strings borrow the
 * input buffer, which lives until sch_on_event returns. */
typedef struct sch_event {
    sch_event_kind kind;
    sch_str name;    /* command */
    sch_str args;    /* command: line-record values, walk with sch_values_next */
    sch_str dialect; /* netlist_request */
    sch_str bench;   /* netlist_request */
    sch_str lang;    /* run_job */
    uint32_t doc;    /* doc_opened, doc_closed, doc_changed, saved */
    uint32_t epoch;  /* doc_changed */
    uint32_t job;    /* run_job, run_done */
    uint32_t handle; /* io_done */
    int32_t status;  /* io_done: >= 0 success (exit status for spawn), < 0 -sch_code */
    uint32_t id;     /* timer */
    uint32_t code;   /* rejected: an sch_code */
    sch_str bytes;   /* io_done payload, rejected record */
} sch_event;

/* Parses the wire form; returns 0 or SCH_E_PARSE. */
int sch_event_parse(sch_str in, sch_event *ev);

/* ---- Line-record values (format spec §2): `bare` or "quoted \"escaped\"" ---- */

/* Copies the next value of *rest, unescaped, into dst; returns its length, -1 when none is
 * left, or -SCH_E_PARSE (malformed, or longer than cap). Advances *rest. */
int32_t sch_values_next(sch_str *rest, char *dst, uint32_t cap);

/* ---- Output (ABI §7) ---- */

/* An append-only text buffer: the event's output records, or any text you compose (a log
 * message, an edit's ops). Once full, `overflow` is set and appends are dropped. */
typedef struct sch_out {
    char *buf;
    uint32_t len, cap;
    int overflow;
} sch_out;

#define SCH_OUT(array) {(array), 0, sizeof(array), 0}

static inline sch_str sch_out_str(const sch_out *o) {
    sch_str s = {o->buf, o->len};
    return s;
}
void sch_put(sch_out *o, sch_str s);              /* raw text */
void sch_put_c(sch_out *o, const char *s);        /* raw NUL-terminated text */
void sch_put_u32(sch_out *o, uint32_t v);         /* decimal */
void sch_put_i32(sch_out *o, int32_t v);          /* decimal */
void sch_put_value(sch_out *o, sch_str v);        /* one value: bare if legal, else quoted */

/* Records. A body is line-record text, one record per line; it is written verbatim. */
void sch_contribute(sch_out *o, const char *kind, sch_str body); /* SCH_KIND_* */
void sch_contribute_scoped(sch_out *o, const char *kind, sch_str scope, sch_str body);
void sch_edit(sch_out *o, uint32_t doc, int stage, sch_str label, sch_str ops);
void sch_run(sch_out *o, sch_str bench, sch_str config, sch_str corner);
void sch_vary(sch_out *o, sch_str var, sch_str value); /* right after sch_run */
void sch_timer(sch_out *o, uint32_t id, uint32_t ms);
void sch_log(sch_out *o, enum sch_level level, sch_str text);

/* ---- What the plugin writes ---- */

void sch_on_event(const sch_event *ev, sch_out *out);
#ifdef SCHEMIFY_FUNCTIONS
/* Pure expression function `fn_id` (D20): args are line-record values; write one
 * canonical value into result. Return 0, or an sch_code. Every import is denied here. */
uint32_t sch_call(uint32_t fn_id, sch_str args, sch_out *result);
#endif

#ifdef __cplusplus
}
#endif

/* ======================================================================================= */
#ifdef SCHEMIFY_IMPLEMENTATION

#ifndef SCHEMIFY_INPUT_MAX
#define SCHEMIFY_INPUT_MAX (1u << 20)
#endif
#ifndef SCHEMIFY_OUTPUT_MAX
#define SCHEMIFY_OUTPUT_MAX (1u << 20)
#endif

#ifdef __cplusplus
extern "C" {
#endif

static int sch__space(char c) { return c == ' ' || c == '\t'; }

/* A bare-value character (format §2): not whitespace, quote, backslash or control. */
static int sch__bchar(unsigned char c) {
    return c > 0x20 && c != 0x7f && c != '"' && c != '\\';
}

/* Next whitespace-separated token of *rest (bare text only: identifiers and numbers). */
static sch_str sch__token(sch_str *rest) {
    uint32_t i = 0;
    while (i < rest->len && sch__space(rest->ptr[i])) i++;
    uint32_t start = i;
    while (i < rest->len && !sch__space(rest->ptr[i])) i++;
    sch_str t = {rest->ptr + start, i - start};
    rest->ptr += i;
    rest->len -= i;
    return t;
}

static int sch__bare(sch_str t) {
    if (t.len == 0 || t.ptr[0] == '#' || t.ptr[0] == '|') return 0;
    for (uint32_t i = 0; i < t.len; i++)
        if (!sch__bchar((unsigned char)t.ptr[i])) return 0;
    return 1;
}

static int sch__num(sch_str t, int64_t lo, int64_t hi, int64_t *v) {
    uint32_t i = 0;
    int neg = t.len > 0 && t.ptr[0] == '-';
    i = (uint32_t)neg;
    if (i == t.len) return 0;
    int64_t n = 0;
    for (; i < t.len; i++) {
        if (t.ptr[i] < '0' || t.ptr[i] > '9' || n > (int64_t)1 << 40) return 0;
        n = n * 10 + (t.ptr[i] - '0');
    }
    if (neg) n = -n;
    if (n < lo || n > hi) return 0;
    *v = n;
    return 1;
}

static int sch__u32(sch_str *rest, uint32_t *v) {
    int64_t n;
    if (!sch__num(sch__token(rest), 0, 0xffffffffLL, &n)) return 0;
    *v = (uint32_t)n;
    return 1;
}

static int sch__ident(sch_str *rest, sch_str *v) {
    *v = sch__token(rest);
    return sch__bare(*v);
}

static sch_event sch__no_event; /* all zero */

int sch_event_parse(sch_str in, sch_event *ev) {
    *ev = sch__no_event;
    sch_str line = in;
    for (uint32_t i = 0; i < in.len; i++) {
        if (in.ptr[i] == 0) {
            line.len = i;
            ev->bytes.ptr = in.ptr + i + 1;
            ev->bytes.len = in.len - i - 1;
            break;
        }
    }
    int payload = ev->bytes.ptr != 0;
    if (line.len > 0 && line.ptr[line.len - 1] == '\n') line.len--;
    for (uint32_t i = 0; i < line.len; i++)
        if (line.ptr[i] == '\n' || line.ptr[i] == '\r') return SCH_E_PARSE;
    sch_str rest = line;
    if (!sch_str_eq(sch__token(&rest), SCH_STR("event"))) return SCH_E_PARSE;
    sch_str k = sch__token(&rest);
    int ok = 1;
    int wants_payload = 0;
    if (sch_str_eq(k, SCH_STR("load"))) ev->kind = SCH_EV_LOAD;
    else if (sch_str_eq(k, SCH_STR("unload"))) ev->kind = SCH_EV_UNLOAD;
    else if (sch_str_eq(k, SCH_STR("selection_changed"))) ev->kind = SCH_EV_SELECTION_CHANGED;
    else if (sch_str_eq(k, SCH_STR("command"))) {
        ev->kind = SCH_EV_COMMAND;
        ok = sch__ident(&rest, &ev->name);
        while (rest.len > 0 && sch__space(rest.ptr[0])) rest.ptr++, rest.len--;
        ev->args = rest;
        rest.len = 0;
    } else if (sch_str_eq(k, SCH_STR("doc_opened"))) {
        ev->kind = SCH_EV_DOC_OPENED;
        ok = sch__u32(&rest, &ev->doc);
    } else if (sch_str_eq(k, SCH_STR("doc_closed"))) {
        ev->kind = SCH_EV_DOC_CLOSED;
        ok = sch__u32(&rest, &ev->doc);
    } else if (sch_str_eq(k, SCH_STR("saved"))) {
        ev->kind = SCH_EV_SAVED;
        ok = sch__u32(&rest, &ev->doc);
    } else if (sch_str_eq(k, SCH_STR("doc_changed"))) {
        ev->kind = SCH_EV_DOC_CHANGED;
        ok = sch__u32(&rest, &ev->doc) && sch__u32(&rest, &ev->epoch);
    } else if (sch_str_eq(k, SCH_STR("netlist_request"))) {
        ev->kind = SCH_EV_NETLIST_REQUEST;
        ok = sch__ident(&rest, &ev->dialect) && sch__ident(&rest, &ev->bench);
    } else if (sch_str_eq(k, SCH_STR("run_job"))) {
        ev->kind = SCH_EV_RUN_JOB;
        ok = sch__u32(&rest, &ev->job) && sch__ident(&rest, &ev->lang);
    } else if (sch_str_eq(k, SCH_STR("run_done"))) {
        ev->kind = SCH_EV_RUN_DONE;
        ok = sch__u32(&rest, &ev->job);
    } else if (sch_str_eq(k, SCH_STR("timer"))) {
        ev->kind = SCH_EV_TIMER;
        ok = sch__u32(&rest, &ev->id);
    } else if (sch_str_eq(k, SCH_STR("io_done"))) {
        int64_t status = 0;
        ev->kind = SCH_EV_IO_DONE;
        wants_payload = 1;
        ok = sch__u32(&rest, &ev->handle) &&
             sch__num(sch__token(&rest), -0x80000000LL, 0x7fffffffLL, &status);
        ev->status = (int32_t)status;
    } else if (sch_str_eq(k, SCH_STR("rejected"))) {
        sch_str name = sch__token(&rest);
        ev->kind = SCH_EV_REJECTED;
        wants_payload = 1;
        for (uint32_t c = 1; c < sizeof sch_code_names / sizeof *sch_code_names; c++)
            if (sch_str_eq(name, sch_cstr(sch_code_names[c]))) ev->code = c;
        ok = ev->code != 0;
    } else {
        return SCH_E_PARSE;
    }
    if (!ok || payload != wants_payload || sch__token(&rest).len != 0) return SCH_E_PARSE;
    return 0;
}

static uint32_t sch__hex(char c) {
    if (c >= '0' && c <= '9') return (uint32_t)(c - '0');
    if (c >= 'a' && c <= 'f') return (uint32_t)(c - 'a' + 10);
    if (c >= 'A' && c <= 'F') return (uint32_t)(c - 'A' + 10);
    return 16;
}

int32_t sch_values_next(sch_str *rest, char *dst, uint32_t cap) {
    uint32_t i = 0, n = 0;
    while (i < rest->len && sch__space(rest->ptr[i])) i++;
    if (i == rest->len) {
        rest->ptr += i;
        rest->len = 0;
        return -1;
    }
    const char *s = rest->ptr;
    uint32_t len = rest->len;
    rest->len = 0; /* stop after an error */
#define SCH__PUSH(c)                              \
    do {                                          \
        if (n == cap) return -(int32_t)SCH_E_PARSE; \
        dst[n++] = (char)(c);                     \
    } while (0)
    if (s[i] == '"') {
        for (i++;; i++) {
            if (i == len || s[i] == '\n' || s[i] == '\r') return -(int32_t)SCH_E_PARSE;
            if (s[i] == '"') break;
            if (s[i] != '\\') {
                SCH__PUSH(s[i]);
                continue;
            }
            if (++i == len) return -(int32_t)SCH_E_PARSE;
            char e = s[i];
            if (e == '"' || e == '\\') SCH__PUSH(e);
            else if (e == 'n') SCH__PUSH('\n');
            else if (e == 't') SCH__PUSH('\t');
            else if (e == 'u' && i + 1 < len && s[i + 1] == '{') {
                uint32_t cp = 0, digits = 0;
                for (i += 2; i < len && s[i] != '}'; i++, digits++) {
                    if (digits == 6 || sch__hex(s[i]) == 16) return -(int32_t)SCH_E_PARSE;
                    cp = cp * 16 + sch__hex(s[i]);
                }
                if (i == len || digits == 0 || cp > 0x10ffff || (cp >= 0xd800 && cp < 0xe000))
                    return -(int32_t)SCH_E_PARSE;
                if (cp < 0x80) SCH__PUSH(cp);
                else if (cp < 0x800) {
                    SCH__PUSH(0xc0 | cp >> 6);
                    SCH__PUSH(0x80 | (cp & 0x3f));
                } else if (cp < 0x10000) {
                    SCH__PUSH(0xe0 | cp >> 12);
                    SCH__PUSH(0x80 | (cp >> 6 & 0x3f));
                    SCH__PUSH(0x80 | (cp & 0x3f));
                } else {
                    SCH__PUSH(0xf0 | cp >> 18);
                    SCH__PUSH(0x80 | (cp >> 12 & 0x3f));
                    SCH__PUSH(0x80 | (cp >> 6 & 0x3f));
                    SCH__PUSH(0x80 | (cp & 0x3f));
                }
            } else
                return -(int32_t)SCH_E_PARSE;
        }
        i++;
    } else {
        if (s[i] == '#' || s[i] == '|') return -(int32_t)SCH_E_PARSE;
        for (; i < len && sch__bchar((unsigned char)s[i]); i++) SCH__PUSH(s[i]);
    }
#undef SCH__PUSH
    if (i < len && !sch__space(s[i])) return -(int32_t)SCH_E_PARSE;
    rest->ptr = s + i;
    rest->len = len - i;
    return (int32_t)n;
}

void sch_put(sch_out *o, sch_str s) {
    if (o->overflow || s.len > o->cap - o->len) {
        o->overflow = 1;
        return;
    }
    for (uint32_t i = 0; i < s.len; i++) o->buf[o->len++] = s.ptr[i];
}

void sch_put_c(sch_out *o, const char *s) { sch_put(o, sch_cstr(s)); }

void sch_put_u32(sch_out *o, uint32_t v) {
    char d[10];
    uint32_t n = sizeof d;
    do d[--n] = (char)('0' + v % 10);
    while (v /= 10);
    sch_str s = {d + n, (uint32_t)sizeof d - n};
    sch_put(o, s);
}

void sch_put_i32(sch_out *o, int32_t v) {
    if (v < 0) sch_put(o, SCH_STR("-"));
    sch_put_u32(o, v < 0 ? 0u - (uint32_t)v : (uint32_t)v);
}

void sch_put_value(sch_out *o, sch_str v) {
    if (sch__bare(v)) {
        sch_put(o, v);
        return;
    }
    static const char hex[] = "0123456789abcdef";
    sch_put(o, SCH_STR("\""));
    for (uint32_t i = 0; i < v.len; i++) {
        unsigned char c = (unsigned char)v.ptr[i];
        if (c == '"') sch_put(o, SCH_STR("\\\""));
        else if (c == '\\') sch_put(o, SCH_STR("\\\\"));
        else if (c == '\n') sch_put(o, SCH_STR("\\n"));
        else if (c == '\t') sch_put(o, SCH_STR("\\t"));
        else if (c < 0x20 || c == 0x7f) {
            char e[7] = {'\\', 'u', '{', hex[c >> 4], hex[c & 15], '}', 0};
            sch_str es = {e, 6};
            if (c < 0x10) { /* no leading zero: \u{a} */
                e[3] = hex[c];
                e[4] = '}';
                es.len = 5;
            }
            sch_put(o, es);
        } else {
            sch_str one = {v.ptr + i, 1};
            sch_put(o, one);
        }
    }
    sch_put(o, SCH_STR("\""));
}

/* Each line of body as `  | line`; an empty line as `  |`. */
static void sch__verbatim(sch_out *o, sch_str body) {
    uint32_t start = 0;
    for (uint32_t i = 0; i <= body.len; i++) {
        if (i < body.len && body.ptr[i] != '\n') continue;
        if (i == body.len && start == i) break;
        sch_str line = {body.ptr + start, i - start};
        if (line.len > 0 && line.ptr[line.len - 1] == '\r') line.len--;
        sch_put(o, line.len ? SCH_STR("  | ") : SCH_STR("  |"));
        sch_put(o, line);
        sch_put(o, SCH_STR("\n"));
        start = i + 1;
    }
}

void sch_contribute(sch_out *o, const char *kind, sch_str body) {
    sch_put(o, SCH_STR("contribute "));
    sch_put_c(o, kind);
    sch_put(o, SCH_STR("\n"));
    sch__verbatim(o, body);
}

void sch_contribute_scoped(sch_out *o, const char *kind, sch_str scope, sch_str body) {
    sch_put(o, SCH_STR("contribute "));
    sch_put_c(o, kind);
    sch_put(o, SCH_STR(" "));
    sch_put_value(o, scope);
    sch_put(o, SCH_STR("\n"));
    sch__verbatim(o, body);
}

void sch_edit(sch_out *o, uint32_t doc, int stage, sch_str label, sch_str ops) {
    sch_put(o, SCH_STR("edit "));
    sch_put_u32(o, doc);
    sch_put(o, stage ? SCH_STR(" stage ") : SCH_STR(" commit "));
    sch_put_value(o, label);
    sch_put(o, SCH_STR("\n"));
    sch__verbatim(o, ops);
}

void sch_run(sch_out *o, sch_str bench, sch_str config, sch_str corner) {
    sch_put(o, SCH_STR("run "));
    sch_put_value(o, bench);
    sch_put(o, SCH_STR(" "));
    sch_put_value(o, config);
    sch_put(o, SCH_STR(" "));
    sch_put_value(o, corner);
    sch_put(o, SCH_STR("\n"));
}

void sch_vary(sch_out *o, sch_str var, sch_str value) {
    sch_put(o, SCH_STR("  vary "));
    sch_put_value(o, var);
    sch_put(o, SCH_STR(" "));
    sch_put_value(o, value);
    sch_put(o, SCH_STR("\n"));
}

void sch_timer(sch_out *o, uint32_t id, uint32_t ms) {
    sch_put(o, SCH_STR("timer "));
    sch_put_u32(o, id);
    sch_put(o, SCH_STR(" "));
    sch_put_u32(o, ms);
    sch_put(o, SCH_STR("\n"));
}

void sch_log(sch_out *o, enum sch_level level, sch_str text) {
    static const char *const names[] = {"error", "warn", "info", "debug"};
    sch_put(o, SCH_STR("log "));
    sch_put_c(o, names[level & 3]);
    sch_put(o, SCH_STR(" "));
    sch_put_value(o, text);
    sch_put(o, SCH_STR("\n"));
}

/* The compiler lowers struct copies and some loops to these; without a libc the plugin
 * supplies them. Weak, so a plugin that links its own wins. */
__attribute__((weak)) void *memset(void *dst, int c, unsigned long n) {
    unsigned char *d = (unsigned char *)dst;
    while (n--) *d++ = (unsigned char)c;
    return dst;
}
__attribute__((weak)) void *memcpy(void *dst, const void *src, unsigned long n) {
    unsigned char *d = (unsigned char *)dst;
    const unsigned char *s = (const unsigned char *)src;
    while (n--) *d++ = *s++;
    return dst;
}
__attribute__((weak)) unsigned long strlen(const char *s) {
    unsigned long n = 0;
    while (s[n]) n++;
    return n;
}
__attribute__((weak)) void *memmove(void *dst, const void *src, unsigned long n) {
    unsigned char *d = (unsigned char *)dst;
    const unsigned char *s = (const unsigned char *)src;
    if (d < s) while (n--) *d++ = *s++;
    else while (n--) d[n] = s[n];
    return dst;
}

/* ---- The exports (ABI §2) ---- */

static char sch__in[SCHEMIFY_INPUT_MAX];
static char sch__out[SCHEMIFY_OUTPUT_MAX];
static uint32_t sch__in_len;

static uint64_t sch__pack(const char *ptr, uint32_t len) {
    return (uint64_t)(uintptr_t)ptr << 32 | len;
}

__attribute__((export_name("schemify_abi"))) uint32_t schemify_abi(void) {
    return SCH_ABI_VERSION;
}

__attribute__((export_name("schemify_alloc"))) uint32_t schemify_alloc(uint32_t len) {
    if (len > sizeof sch__in) return 0; /* the host reports E_SPACE */
    sch__in_len = len;
    return (uint32_t)(uintptr_t)sch__in;
}

__attribute__((export_name("schemify_on_event"))) uint64_t schemify_on_event(uint32_t ptr,
                                                                              uint32_t len) {
    sch_out out = SCH_OUT(sch__out);
    sch_event ev;
    sch_str in = {sch__in, len};
    if (ptr != (uint32_t)(uintptr_t)sch__in || len != sch__in_len) return 0;
    if (sch_event_parse(in, &ev) != 0) sch_log(&out, SCH_ERROR, SCH_STR("unparsable event"));
    else sch_on_event(&ev, &out);
    if (out.overflow) {
        out.len = 0;
        out.overflow = 0;
        sch_log(&out, SCH_ERROR, SCH_STR("output over SCHEMIFY_OUTPUT_MAX, dropped"));
    }
    return out.len ? sch__pack(out.buf, out.len) : 0;
}

#ifdef SCHEMIFY_FUNCTIONS
__attribute__((export_name("schemify_call"))) uint64_t schemify_call(uint32_t fn_id,
                                                                      uint32_t ptr,
                                                                      uint32_t len) {
    sch_out result = SCH_OUT(sch__out);
    if (ptr != (uint32_t)(uintptr_t)sch__in || len != sch__in_len) return sch__pack(0, SCH_E_RANGE);
    sch_str args = {sch__in, len};
    uint32_t code = sch_call(fn_id, args, &result);
    if (code == 0 && (result.overflow || result.len == 0)) code = result.overflow ? SCH_E_SPACE : SCH_E_PARSE;
    return code ? sch__pack(0, code) : sch__pack(result.buf, result.len);
}
#endif

#ifdef __cplusplus
}
#endif

#endif /* SCHEMIFY_IMPLEMENTATION */
#endif /* SCHEMIFY_H */
