// Package schemify is the Schemify plugin ABI v1 for Go (docs/spec/plugin-abi.md), built
// with TinyGo's wasm-unknown target (standard Go's wasm targets import WASI or gojs, which
// the host refuses):
//
//	tinygo build -target=wasm-unknown -no-debug -o plugin.wasm .
//
// The package provides the module's exports (schemify_abi, schemify_alloc,
// schemify_on_event, schemify_call); the plugin registers its handlers from an init
// function and keeps an empty main, which is never called:
//
//	func init() { schemify.Handle(onEvent) }
//	func main() {}
//
// wasm-unknown's garbage collector never frees, so nothing here allocates per event: the
// buffers are static, strings borrow them, and the plugin should do the same (fixed arrays,
// Out builders over them). An event over InputMax bytes is refused (the host reports
// E_SPACE), and output past OutputMax is replaced by one error log.
package schemify

import (
	"strings"
	"unicode/utf8"
	"unsafe"
)

// ---- ABI numbers, from crates/protocol/abi (cargo test -p schemify-abi --test sdk) ----
// BEGIN GENERATED
// AbiVersion is what schemify_abi returns.
const AbiVersion = 1

// Code is an error code (ABI §8); imports return them negated.
type Code uint32

const (
	EDenied Code = 1
	EGone Code = 2
	ESpace Code = 3
	ETable Code = 4
	EColumn Code = 5
	ERange Code = 6
	EParse Code = 7
	EFuel Code = 8
	EVersion Code = 9
	ETrap Code = 10
	EModule Code = 11
)

var codeNames = [...]string{"", "E_DENIED", "E_GONE", "E_SPACE", "E_TABLE", "E_COLUMN", "E_RANGE", "E_PARSE", "E_FUEL", "E_VERSION", "E_TRAP", "E_MODULE"}

// Table is a snapshot table id (ABI §4).
type Table uint32

const (
	Docs Table = 0
	Instances Table = 1
	Wires Table = 2
	Labels Table = 3
	Nets Table = 4
	InstParams Table = 5
	Paths Table = 6
	NameMap Table = 7
	Violations Table = 8
	DocVars Table = 9
	BenchSpecs Table = 10
	BenchOutputs Table = 11
	Results Table = 12
	Selection Table = 13
	UiNodes Table = 14
)

// Column ids, <Table><Column>.
const (
	DocsHandle uint32 = 0 // u32
	DocsKind uint32 = 1 // u8
	DocsDirty uint32 = 2 // u8
	DocsPath uint32 = 3 // str
	DocsCell uint32 = 4 // str
	InstancesDoc uint32 = 0 // u32
	InstancesId uint32 = 1 // u32
	InstancesX uint32 = 2 // i32
	InstancesY uint32 = 3 // i32
	InstancesOrient uint32 = 4 // u8
	InstancesCell uint32 = 5 // str
	InstancesName uint32 = 6 // str
	WiresDoc uint32 = 0 // u32
	WiresId uint32 = 1 // u32
	WiresX1 uint32 = 2 // i32
	WiresY1 uint32 = 3 // i32
	WiresX2 uint32 = 4 // i32
	WiresY2 uint32 = 5 // i32
	WiresNet uint32 = 6 // u32
	LabelsDoc uint32 = 0 // u32
	LabelsId uint32 = 1 // u32
	LabelsX uint32 = 2 // i32
	LabelsY uint32 = 3 // i32
	LabelsNet uint32 = 4 // u32
	LabelsText uint32 = 5 // str
	NetsName uint32 = 0 // str
	NetsWidth uint32 = 1 // u16
	NetsGlobal uint32 = 2 // u8
	NetsPinCount uint32 = 3 // u32
	InstParamsInstRow uint32 = 0 // u32
	InstParamsName uint32 = 1 // str
	InstParamsValue uint32 = 2 // str
	InstParamsSource uint32 = 3 // u8
	PathsPath uint32 = 0 // str
	PathsCell uint32 = 1 // str
	PathsImpl uint32 = 2 // str
	PathsParent uint32 = 3 // u32
	NameMapPath uint32 = 0 // str
	NameMapEmitted uint32 = 1 // str
	ViolationsCode uint32 = 0 // str
	ViolationsSeverity uint32 = 1 // u8
	ViolationsObject uint32 = 2 // u32
	ViolationsParam uint32 = 3 // str
	DocVarsName uint32 = 0 // str
	DocVarsExpr uint32 = 1 // str
	DocVarsValue uint32 = 2 // str
	BenchSpecsName uint32 = 0 // str
	BenchSpecsExpr uint32 = 1 // str
	BenchSpecsMin uint32 = 2 // str
	BenchSpecsMax uint32 = 3 // str
	BenchOutputsName uint32 = 0 // str
	BenchOutputsExpr uint32 = 1 // str
	BenchOutputsMin uint32 = 2 // str
	BenchOutputsMax uint32 = 3 // str
	ResultsJob uint32 = 0 // u32
	ResultsOutput uint32 = 1 // str
	ResultsValue uint32 = 2 // str
	ResultsPass uint32 = 3 // u8
	SelectionDoc uint32 = 0 // u32
	SelectionId uint32 = 1 // u32
	UiNodesId uint32 = 0 // str
	UiNodesKind uint32 = 1 // u8
	UiNodesParent uint32 = 2 // str
	UiNodesOwner uint32 = 3 // u16
)

// Level is a log level.
type Level uint32

const (
	LevelError Level = 0
	LevelWarn Level = 1
	LevelInfo Level = 2
	LevelDebug Level = 3
)

// Kind is a contribution kind (ABI §1).
type Kind string

const (
	KindUi Kind = "ui"
	KindCommands Kind = "commands"
	KindKeymap Kind = "keymap"
	KindAnnotations Kind = "annotations"
	KindFunctions Kind = "functions"
	KindDialects Kind = "dialects"
	KindRunners Kind = "runners"
	KindChecks Kind = "checks"
	KindConstraintTypes Kind = "constraint_types"
)
// END GENERATED

// Error returns the code's ABI name (E_PARSE, ...), so a Code is the error every import
// and parser here returns.
func (c Code) Error() string {
	if c == 0 || int(c) >= len(codeNames) {
		return "E_?"
	}
	return codeNames[c]
}

// ---- Host imports (ABI §3, §5). Raw returns are negative -Code on failure. ----

//go:wasmimport schemify table_len
func tableLen(table uint32) int32

//go:wasmimport schemify table_epoch
func tableEpoch(table uint32) int32

//go:wasmimport schemify read_column
func readColumn(table, column, start, count uint32, dst unsafe.Pointer, dstLen uint32) int32

//go:wasmimport schemify read_strings
func readStrings(table, column, start, count uint32, dst unsafe.Pointer, dstLen uint32) int32

//go:wasmimport schemify eval
func eval(doc uint32, expr unsafe.Pointer, n uint32) int32

//go:wasmimport schemify log
func hostLog(level uint32, text unsafe.Pointer, n uint32)

//go:wasmimport schemify fs_read
func fsRead(path unsafe.Pointer, n uint32) int32

//go:wasmimport schemify fs_list
func fsList(path unsafe.Pointer, n uint32) int32

//go:wasmimport schemify fs_write
func fsWrite(path unsafe.Pointer, n uint32, data unsafe.Pointer, dataLen uint32) int32

//go:wasmimport schemify fs_remove
func fsRemove(path unsafe.Pointer, n uint32) int32

//go:wasmimport schemify spawn
func spawn(argv unsafe.Pointer, n uint32, stdin unsafe.Pointer, stdinLen uint32) int32

//go:wasmimport schemify fetch
func fetch(url unsafe.Pointer, n uint32, body unsafe.Pointer, bodyLen uint32) int32

//go:wasmimport schemify clipboard_get
func clipboardGet() int32

//go:wasmimport schemify clipboard_set
func clipboardSet(text unsafe.Pointer, n uint32) int32

func result(r int32) (int, error) {
	if r < 0 {
		return 0, Code(-r)
	}
	return int(r), nil
}

func handle(r int32) (uint32, error) {
	n, err := result(r)
	return uint32(n), err
}

func sp(s string) (unsafe.Pointer, uint32) {
	return unsafe.Pointer(unsafe.StringData(s)), uint32(len(s))
}

func bp(b []byte) (unsafe.Pointer, uint32) {
	return unsafe.Pointer(unsafe.SliceData(b)), uint32(len(b))
}

// str borrows b as a string without copying; b must not change while the string is used.
func str(b []byte) string { return unsafe.String(unsafe.SliceData(b), len(b)) }

// TableLen is the row count of a snapshot table (at most 2^24).
func TableLen(t Table) (int, error) { return result(tableLen(uint32(t))) }

// TableEpoch changes whenever the table changes (31-bit, wraps): a cheap staleness check.
func TableEpoch(t Table) (int, error) { return result(tableEpoch(uint32(t))) }

// Fixed is an element type of a fixed-width column (the column comments in the generated
// list name each column's type; using another width reads garbage, not an error).
type Fixed interface {
	~uint8 | ~uint16 | ~uint32 | ~int32
}

// ReadColumn copies rows start..start+len(dst) of a fixed-width column into dst and returns
// the rows read. ECOLUMN for a string column, ERANGE past the table's end.
func ReadColumn[T Fixed](t Table, column, start uint32, dst []T) (int, error) {
	size := uint32(unsafe.Sizeof(*new(T)))
	n, err := result(readColumn(uint32(t), column, start, uint32(len(dst)), unsafe.Pointer(unsafe.SliceData(dst)), uint32(len(dst))*size))
	return n / int(size), err
}

// ReadStrings copies `count` rows of a string column into dst and returns a walker over
// them, which borrows dst. On ESpace the size needed is the u32 LE in dst's first 4 bytes.
func ReadStrings(t Table, column, start, count uint32, dst []byte) (Strings, error) {
	p, n := bp(dst)
	got, err := result(readStrings(uint32(t), column, start, count, p, n))
	return Strings{buf: dst[:got]}, err
}

// Strings walks a ReadStrings buffer: each string is a u32 LE length and its UTF-8 bytes.
type Strings struct {
	buf []byte
	at  int
}

// NewStrings walks buf, the bytes a read_strings call wrote.
func NewStrings(buf []byte) Strings { return Strings{buf: buf} }

// Next returns the next string, borrowing the buffer; false when none is left or the
// rest is truncated.
func (s *Strings) Next() (string, bool) {
	b := s.buf[s.at:]
	if len(b) < 4 {
		return "", false
	}
	n := int(uint32(b[0]) | uint32(b[1])<<8 | uint32(b[2])<<16 | uint32(b[3])<<24)
	if n > len(b)-4 {
		return "", false
	}
	s.at += 4 + n
	return str(b[4 : 4+n]), true
}

// Eval evaluates expr in document doc; the value arrives later as an EventIoDone with the
// returned handle.
func Eval(doc uint32, expr string) (uint32, error) {
	p, n := sp(expr)
	return handle(eval(doc, p, n))
}

// Log writes to the host's log at once; it does nothing outside an event. Out.Log instead
// queues a log record with the event's other outputs.
func Log(level Level, text string) {
	p, n := sp(text)
	hostLog(uint32(level), p, n)
}

// The capability imports need a grant in schemify.project (EDenied otherwise). Each returns
// a handle; the result arrives as an EventIoDone with Event.Bytes holding the data.

// FsRead reads a file under the project root.
func FsRead(path string) (uint32, error) { p, n := sp(path); return handle(fsRead(p, n)) }

// FsList lists a directory under the project root.
func FsList(path string) (uint32, error) { p, n := sp(path); return handle(fsList(p, n)) }

// FsWrite replaces a file under the project root with data.
func FsWrite(path string, data []byte) (uint32, error) {
	p, n := sp(path)
	d, dn := bp(data)
	return handle(fsWrite(p, n, d, dn))
}

// FsRemove removes a file under the project root.
func FsRemove(path string) (uint32, error) { p, n := sp(path); return handle(fsRemove(p, n)) }

// Spawn runs a program (argv as line-record values, argv[0] found on PATH) with stdin;
// Event.Status is its exit status and Event.Bytes its stdout then stderr.
func Spawn(argv string, stdin []byte) (uint32, error) {
	p, n := sp(argv)
	d, dn := bp(stdin)
	return handle(spawn(p, n, d, dn))
}

// Fetch requests url, POSTing body when it is non-empty.
func Fetch(url string, body []byte) (uint32, error) {
	p, n := sp(url)
	d, dn := bp(body)
	return handle(fetch(p, n, d, dn))
}

// ClipboardGet reads the clipboard text.
func ClipboardGet() (uint32, error) { return handle(clipboardGet()) }

// ClipboardSet replaces the clipboard text.
func ClipboardSet(text string) (uint32, error) { p, n := sp(text); return handle(clipboardSet(p, n)) }

// ---- Events (ABI §6) ----

// EventKind says which Event fields are set.
type EventKind uint8

const (
	EventLoad EventKind = iota
	EventUnload
	EventCommand
	EventDocOpened
	EventDocClosed
	EventDocChanged
	EventSelectionChanged
	EventNetlistRequest
	EventRunJob
	EventRunDone
	EventIoDone
	EventTimer
	EventSaved
	EventRejected
)

// Event is one parsed event; the fields its kind uses are set, the rest are zero. Strings
// and Bytes borrow the input buffer, which the next event overwrites: copy what you keep.
type Event struct {
	Kind    EventKind
	Name    string // command
	Args    string // command: line-record values, walk with NewValues
	Dialect string // netlist_request
	Bench   string // netlist_request
	Lang    string // run_job
	Doc     uint32 // doc_opened, doc_closed, doc_changed, saved
	Epoch   uint32 // doc_changed
	Job     uint32 // run_job, run_done
	Handle  uint32 // io_done
	Status  int32  // io_done: >= 0 success (exit status for spawn), < 0 -Code
	ID      uint32 // timer
	Code    Code   // rejected
	Bytes   []byte // io_done payload, rejected record
}

func space(c byte) bool { return c == ' ' || c == '\t' }

// bchar is a bare-value character (format §2): not whitespace, quote, backslash or control.
func bchar(c byte) bool { return c > 0x20 && c != 0x7f && c != '"' && c != '\\' }

// token is the next whitespace-separated token of *rest (bare text only).
func token(rest *string) string {
	s, i := *rest, 0
	for i < len(s) && space(s[i]) {
		i++
	}
	start := i
	for i < len(s) && !space(s[i]) {
		i++
	}
	*rest = s[i:]
	return s[start:i]
}

func bare(t string) bool {
	if t == "" || t[0] == '#' || t[0] == '|' {
		return false
	}
	for i := 0; i < len(t); i++ {
		if !bchar(t[i]) {
			return false
		}
	}
	return true
}

func num(t string, lo, hi int64) (int64, bool) {
	i := 0
	if t != "" && t[0] == '-' {
		i = 1
	}
	if i == len(t) {
		return 0, false
	}
	var n int64
	for j := i; j < len(t); j++ {
		if t[j] < '0' || t[j] > '9' || n > 1<<40 {
			return 0, false
		}
		n = n*10 + int64(t[j]-'0')
	}
	if i == 1 {
		n = -n
	}
	return n, n >= lo && n <= hi
}

func u32(rest *string) (uint32, bool) {
	n, ok := num(token(rest), 0, 0xffffffff)
	return uint32(n), ok
}

func ident(rest *string) (string, bool) {
	t := token(rest)
	return t, bare(t)
}

// ParseEvent parses the wire form; the result borrows in. EParse for anything malformed.
func ParseEvent(in []byte) (Event, error) {
	var ev Event
	line := str(in)
	payload := false
	if i := strings.IndexByte(line, 0); i >= 0 {
		line, ev.Bytes, payload = line[:i], in[i+1:], true
	}
	line = strings.TrimSuffix(line, "\n")
	if strings.ContainsAny(line, "\n\r") {
		return Event{}, EParse
	}
	rest := line
	if token(&rest) != "event" {
		return Event{}, EParse
	}
	ok, wants := true, false
	switch token(&rest) {
	case "load":
		ev.Kind = EventLoad
	case "unload":
		ev.Kind = EventUnload
	case "selection_changed":
		ev.Kind = EventSelectionChanged
	case "command":
		ev.Kind = EventCommand
		ev.Name, ok = ident(&rest)
		ev.Args, rest = strings.TrimLeft(rest, " \t"), ""
	case "doc_opened":
		ev.Kind = EventDocOpened
		ev.Doc, ok = u32(&rest)
	case "doc_closed":
		ev.Kind = EventDocClosed
		ev.Doc, ok = u32(&rest)
	case "saved":
		ev.Kind = EventSaved
		ev.Doc, ok = u32(&rest)
	case "doc_changed":
		ev.Kind = EventDocChanged
		if ev.Doc, ok = u32(&rest); ok {
			ev.Epoch, ok = u32(&rest)
		}
	case "netlist_request":
		ev.Kind = EventNetlistRequest
		if ev.Dialect, ok = ident(&rest); ok {
			ev.Bench, ok = ident(&rest)
		}
	case "run_job":
		ev.Kind = EventRunJob
		if ev.Job, ok = u32(&rest); ok {
			ev.Lang, ok = ident(&rest)
		}
	case "run_done":
		ev.Kind = EventRunDone
		ev.Job, ok = u32(&rest)
	case "timer":
		ev.Kind = EventTimer
		ev.ID, ok = u32(&rest)
	case "io_done":
		ev.Kind, wants = EventIoDone, true
		if ev.Handle, ok = u32(&rest); ok {
			var status int64
			status, ok = num(token(&rest), -0x80000000, 0x7fffffff)
			ev.Status = int32(status)
		}
	case "rejected":
		ev.Kind, wants = EventRejected, true
		name := token(&rest)
		for c := 1; c < len(codeNames); c++ {
			if name == codeNames[c] {
				ev.Code = Code(c)
			}
		}
		ok = ev.Code != 0
	default:
		return Event{}, EParse
	}
	if !ok || payload != wants || token(&rest) != "" {
		return Event{}, EParse
	}
	return ev, nil
}

// ---- Line-record values (format spec §2): `bare` or "quoted \"escaped\"" ----

// Values walks a line of line-record values, like bufio.Scanner: call Next until it
// returns false, then Err.
type Values struct {
	rest string
	err  error
}

// NewValues walks line (a command's Args, a function's arguments).
func NewValues(line string) Values { return Values{rest: line} }

// Err is EParse once a value was malformed or longer than the dst given to Next, else nil.
func (v *Values) Err() error { return v.err }

// sink appends to a fixed slice; past its capacity it only records the overflow.
type sink struct {
	d    []byte
	over bool
}

func (s *sink) put(c byte) {
	if len(s.d) == cap(s.d) {
		s.over = true
		return
	}
	s.d = append(s.d, c)
}

func hex(c byte) uint32 {
	switch {
	case c >= '0' && c <= '9':
		return uint32(c - '0')
	case c >= 'a' && c <= 'f':
		return uint32(c - 'a' + 10)
	case c >= 'A' && c <= 'F':
		return uint32(c - 'A' + 10)
	}
	return 16
}

func (v *Values) fail() ([]byte, bool) {
	v.err = EParse
	return nil, false
}

// Next unescapes the next value into dst[:0] (never past cap(dst), so it never allocates)
// and returns it; false when none is left or on an error, after which Next stays false.
func (v *Values) Next(dst []byte) ([]byte, bool) {
	s, i := v.rest, 0
	v.rest = "" // stop after an error
	for i < len(s) && space(s[i]) {
		i++
	}
	if i == len(s) {
		return nil, false
	}
	out := sink{d: dst[:0]}
	if s[i] == '"' {
		for i++; ; i++ {
			if i == len(s) || s[i] == '\n' || s[i] == '\r' {
				return v.fail()
			}
			if s[i] == '"' {
				break
			}
			if s[i] != '\\' {
				out.put(s[i])
				continue
			}
			if i++; i == len(s) {
				return v.fail()
			}
			switch e := s[i]; {
			case e == '"' || e == '\\':
				out.put(e)
			case e == 'n':
				out.put('\n')
			case e == 't':
				out.put('\t')
			case e == 'u' && i+1 < len(s) && s[i+1] == '{':
				cp, digits := uint32(0), 0
				for i += 2; i < len(s) && s[i] != '}'; i, digits = i+1, digits+1 {
					if digits == 6 || hex(s[i]) == 16 {
						return v.fail()
					}
					cp = cp*16 + hex(s[i])
				}
				if i == len(s) || digits == 0 || cp > 0x10ffff || (cp >= 0xd800 && cp < 0xe000) {
					return v.fail()
				}
				var u [4]byte
				for _, b := range u[:utf8.EncodeRune(u[:], rune(cp))] {
					out.put(b)
				}
			default:
				return v.fail()
			}
		}
		i++
	} else {
		if s[i] == '#' || s[i] == '|' {
			return v.fail()
		}
		for ; i < len(s) && bchar(s[i]); i++ {
			out.put(s[i])
		}
	}
	if out.over || (i < len(s) && !space(s[i])) {
		return v.fail()
	}
	v.rest = s[i:]
	return out.d, true
}

// ---- Output (ABI §7) ----

// Out is an append-only text builder over a fixed buffer: the event's output records, or
// any text you compose (a log message, an edit's ops). Once full it drops every later
// append and Overflowed reports it; it never allocates.
type Out struct {
	buf      []byte
	overflow bool
}

// NewOut builds into buf, whose length is the capacity.
func NewOut(buf []byte) Out { return Out{buf: buf[:0]} }

// Text is the text so far, borrowing the buffer: valid until the next Reset.
func (o *Out) Text() string { return str(o.buf) }

// Bytes is the text so far, borrowing the buffer.
func (o *Out) Bytes() []byte { return o.buf }

// Overflowed is true once an append did not fit; the text then misses it and all later ones.
func (o *Out) Overflowed() bool { return o.overflow }

// Reset empties the builder and clears the overflow.
func (o *Out) Reset() { o.buf, o.overflow = o.buf[:0], false }

// Put appends raw text.
func (o *Out) Put(s string) {
	if o.overflow || len(s) > cap(o.buf)-len(o.buf) {
		o.overflow = true
		return
	}
	o.buf = append(o.buf, s...)
}

// PutBytes appends raw bytes.
func (o *Out) PutBytes(b []byte) { o.Put(str(b)) }

func (o *Out) putByte(c byte) {
	if o.overflow || len(o.buf) == cap(o.buf) {
		o.overflow = true
		return
	}
	o.buf = append(o.buf, c)
}

// PutU32 appends v in decimal.
func (o *Out) PutU32(v uint32) {
	var d [10]byte
	n := len(d)
	for {
		n--
		d[n] = byte('0' + v%10)
		if v /= 10; v == 0 {
			break
		}
	}
	for _, c := range d[n:] {
		o.putByte(c)
	}
}

// PutI32 appends v in decimal.
func (o *Out) PutI32(v int32) {
	if v < 0 {
		o.putByte('-')
		o.PutU32(-uint32(v))
		return
	}
	o.PutU32(uint32(v))
}

// PutValue appends one line-record value: bare when legal, else quoted with \" \\ \n \t
// escaped and other control characters as \u{hex}.
func (o *Out) PutValue(v string) {
	if bare(v) {
		o.Put(v)
		return
	}
	const digits = "0123456789abcdef"
	o.putByte('"')
	for i := 0; i < len(v); i++ {
		switch c := v[i]; {
		case c == '"' || c == '\\':
			o.putByte('\\')
			o.putByte(c)
		case c == '\n':
			o.Put(`\n`)
		case c == '\t':
			o.Put(`\t`)
		case c < 0x20 || c == 0x7f:
			o.Put(`\u{`)
			if c >= 0x10 { // no leading zero: \u{a}
				o.putByte(digits[c>>4])
			}
			o.putByte(digits[c&15])
			o.putByte('}')
		default:
			o.putByte(c)
		}
	}
	o.putByte('"')
}

// verbatim writes each line of body as `  | line` (an empty line as `  |`).
func (o *Out) verbatim(body string) {
	start := 0
	for i := 0; i <= len(body); i++ {
		if i < len(body) && body[i] != '\n' {
			continue
		}
		if i == len(body) && start == i {
			break
		}
		line := strings.TrimSuffix(body[start:i], "\r")
		if line == "" {
			o.Put("  |\n")
		} else {
			o.Put("  | ")
			o.Put(line)
			o.putByte('\n')
		}
		start = i + 1
	}
}

// Contribute replaces this plugin's set of kind with body: line-record text, one record
// per line, written verbatim. Contributions are immediate-mode: send the whole set.
func (o *Out) Contribute(kind Kind, body string) {
	o.Put("contribute ")
	o.Put(string(kind))
	o.putByte('\n')
	o.verbatim(body)
}

// ContributeScoped is Contribute for kinds kept per scope (annotations).
func (o *Out) ContributeScoped(kind Kind, scope, body string) {
	o.Put("contribute ")
	o.Put(string(kind))
	o.putByte(' ')
	o.PutValue(scope)
	o.putByte('\n')
	o.verbatim(body)
}

// EditMode says whether an edit commits (one undo step) or stages a preview.
type EditMode uint8

const (
	Commit EditMode = iota
	Stage
)

// Edit requests a transaction on document doc; ops is line-record text, one op per line.
func (o *Out) Edit(doc uint32, mode EditMode, label, ops string) {
	o.Put("edit ")
	o.PutU32(doc)
	if mode == Stage {
		o.Put(" stage ")
	} else {
		o.Put(" commit ")
	}
	o.PutValue(label)
	o.putByte('\n')
	o.verbatim(ops)
}

// Run requests a job; Vary records written right after it set its variables. The job's
// number arrives in EventRunJob, its end in EventRunDone.
func (o *Out) Run(bench, config, corner string) {
	o.Put("run ")
	o.PutValue(bench)
	o.putByte(' ')
	o.PutValue(config)
	o.putByte(' ')
	o.PutValue(corner)
	o.putByte('\n')
}

// Vary sets one variable of the Run written just before.
func (o *Out) Vary(variable, value string) {
	o.Put("  vary ")
	o.PutValue(variable)
	o.putByte(' ')
	o.PutValue(value)
	o.putByte('\n')
}

// Timer requests an EventTimer with id after ms milliseconds.
func (o *Out) Timer(id, ms uint32) {
	o.Put("timer ")
	o.PutU32(id)
	o.putByte(' ')
	o.PutU32(ms)
	o.putByte('\n')
}

var levelNames = [4]string{"error", "warn", "info", "debug"}

// Log writes a log record.
func (o *Out) Log(level Level, text string) {
	o.Put("log ")
	o.Put(levelNames[level&3])
	o.putByte(' ')
	o.PutValue(text)
	o.putByte('\n')
}

// ---- The exports (ABI §2) ----

// Buffer sizes; fixed at build time.
const (
	InputMax  = 1 << 20
	OutputMax = 1 << 20
)

var (
	input    [InputMax]byte
	inputLen uint32
	output   [OutputMax]byte
	// Static, so passing them to the handler never moves them to the heap.
	event       Event
	out         Out
	onEvent     func(ev *Event, out *Out)
	onCall      func(fnID uint32, args string, result *Out) Code
	initialized bool
)

// Handle registers the event handler; call it from an init function. It writes its outputs
// into out, which is empty on entry.
func Handle(f func(ev *Event, out *Out)) { onEvent = f }

// HandleCall registers the handler of the plugin's pure expression functions (D20): args
// are line-record values; write one canonical value into result and return 0, or a Code.
// Every import is denied there. Without one, every call answers ERange.
func HandleCall(f func(fnID uint32, args string, result *Out) Code) { onCall = f }

// TinyGo's reactor entry (heap, package init). The host never calls it, so the first
// export does.
//
//export _initialize
func initialize()

func start() {
	if !initialized {
		initialized = true
		initialize()
	}
}

func addr(b []byte) uint32 { return uint32(uintptr(unsafe.Pointer(unsafe.SliceData(b)))) }

func pack(ptr, n uint32) uint64 { return uint64(ptr)<<32 | uint64(n) }

//export schemify_abi
func schemifyAbi() uint32 {
	start()
	return AbiVersion
}

//export schemify_alloc
func schemifyAlloc(n uint32) uint32 {
	if n > InputMax {
		return 0 // the host reports E_SPACE
	}
	inputLen = n
	return addr(input[:])
}

//export schemify_on_event
func schemifyOnEvent(ptr, n uint32) uint64 {
	start()
	out = NewOut(output[:])
	if ptr != addr(input[:]) || n != inputLen {
		return 0
	}
	var err error
	if event, err = ParseEvent(input[:n]); err != nil {
		out.Log(LevelError, "unparsable event")
	} else if onEvent != nil {
		onEvent(&event, &out)
	}
	if out.overflow {
		out.Reset()
		out.Log(LevelError, "output over schemify.OutputMax, dropped")
	}
	if len(out.buf) == 0 {
		return 0
	}
	return pack(addr(output[:]), uint32(len(out.buf)))
}

//export schemify_call
func schemifyCall(fnID, ptr, n uint32) uint64 {
	start()
	out = NewOut(output[:])
	if ptr != addr(input[:]) || n != inputLen || onCall == nil {
		return pack(0, uint32(ERange))
	}
	code := onCall(fnID, str(input[:n]), &out)
	if code == 0 && out.overflow {
		code = ESpace
	} else if code == 0 && len(out.buf) == 0 {
		code = EParse
	}
	if code != 0 {
		return pack(0, uint32(code))
	}
	return pack(addr(output[:]), uint32(len(out.buf)))
}
