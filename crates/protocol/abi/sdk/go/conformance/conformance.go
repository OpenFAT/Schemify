// The Go library's conformance guest (../../CONFORMANCE.md). Build from sdk/go:
//
//	tinygo build -target=wasm-unknown -no-debug -o conformance/conformance.wasm ./conformance
package main

import schemify "github.com/OmarSiwy/Schemify/crates/protocol/abi/sdk/go"

// Static buffers: wasm-unknown's heap never frees.
var (
	textBuf  [1 << 16]byte
	value    [4096]byte
	pins     [64]uint32
	names    [4096]byte
	greetBuf [1100]byte
	numBuf   [24]byte
)

func init() {
	schemify.Handle(onEvent)
	schemify.HandleCall(call)
}

func main() {}

func echoArgs(t *schemify.Out, args string) {
	vs := schemify.NewValues(args)
	for {
		v, ok := vs.Next(value[:])
		if !ok {
			break
		}
		t.Put(" [")
		t.PutBytes(v)
		t.Put("]")
	}
	if vs.Err() != nil {
		t.Put(" !parse")
	}
}

func outRecords(out *schemify.Out) {
	out.ContributeScoped(schemify.KindAnnotations, "erc pass", `mark R1 warn "hi"`)
	out.Edit(1, schemify.Stage, "size it", "set_attr M1 w 2u")
	out.Edit(1, schemify.Commit, "move", "set_attr R1 at 10 20\nset_attr R2 at 0 0")
	out.Run("lib:tb", "default", "tt")
	out.Vary("vdd", "1.8")
	out.Vary("w", "two words")
	out.Timer(7, 250)
	out.Log(schemify.LevelWarn, "q\"\\\tx\ny\x01")
	out.Log(schemify.LevelInfo, "done")
}

func code(err error) int32 {
	if c, ok := err.(schemify.Code); ok {
		return int32(c)
	}
	return 0
}

func readNets(out, t *schemify.Out) {
	n, _ := schemify.TableLen(schemify.Nets)
	if n > len(pins) {
		n = 0
	}
	schemify.ReadColumn(schemify.Nets, schemify.NetsPinCount, 0, pins[:n])
	strs, err := schemify.ReadStrings(schemify.Nets, schemify.NetsName, 0, uint32(n), names[:])
	t.Put("nets ")
	t.PutU32(uint32(n))
	t.Put(":")
	for i := 0; i < n && err == nil; i++ {
		name, ok := strs.Next()
		if !ok {
			break
		}
		t.Put(" ")
		t.Put(name)
		t.Put("=")
		t.PutU32(pins[i])
	}
	out.Log(schemify.LevelInfo, t.Text())
	t.Reset()
	t.Put("errors ")
	_, err = schemify.TableLen(99)
	t.PutI32(code(err))
	t.Put(" ")
	_, err = schemify.ReadColumn(schemify.Nets, schemify.NetsName, 0, pins[:n])
	t.PutI32(code(err))
	t.Put(" ")
	_, err = schemify.ReadColumn(schemify.Nets, schemify.NetsPinCount, 0, pins[:n+1])
	t.PutI32(code(err))
}

func onEvent(ev *schemify.Event, out *schemify.Out) {
	t := schemify.NewOut(textBuf[:])
	switch ev.Kind {
	case schemify.EventLoad:
		out.Contribute(schemify.KindCommands, "command conf.echo \"Echo\"\ncommand conf.out \"Out\"\ncommand conf.read \"Read\"")
		out.Contribute(schemify.KindFunctions, "function conf.add 0 2\nfunction conf.greet 1 1")
		t.Put("load")
	case schemify.EventUnload:
		t.Put("unload")
	case schemify.EventSelectionChanged:
		t.Put("selection_changed")
	case schemify.EventCommand:
		switch ev.Name {
		case "conf.out":
			outRecords(out)
			return
		case "conf.read":
			readNets(out, &t)
		default:
			t.Put("command ")
			t.Put(ev.Name)
			echoArgs(&t, ev.Args)
		}
	case schemify.EventDocOpened:
		t.Put("doc_opened ")
		t.PutU32(ev.Doc)
	case schemify.EventDocClosed:
		t.Put("doc_closed ")
		t.PutU32(ev.Doc)
	case schemify.EventSaved:
		t.Put("saved ")
		t.PutU32(ev.Doc)
	case schemify.EventDocChanged:
		t.Put("doc_changed ")
		t.PutU32(ev.Doc)
		t.Put(" ")
		t.PutU32(ev.Epoch)
	case schemify.EventNetlistRequest:
		t.Put("netlist_request ")
		t.Put(ev.Dialect)
		t.Put(" ")
		t.Put(ev.Bench)
	case schemify.EventRunJob:
		t.Put("run_job ")
		t.PutU32(ev.Job)
		t.Put(" ")
		t.Put(ev.Lang)
	case schemify.EventRunDone:
		t.Put("run_done ")
		t.PutU32(ev.Job)
	case schemify.EventTimer:
		t.Put("timer ")
		t.PutU32(ev.ID)
	case schemify.EventIoDone:
		t.Put("io_done ")
		t.PutU32(ev.Handle)
		t.Put(" ")
		t.PutI32(ev.Status)
		t.Put(" ")
		t.PutBytes(ev.Bytes)
	case schemify.EventRejected:
		t.Put("rejected ")
		t.PutU32(uint32(ev.Code))
		t.Put(" ")
		t.PutBytes(ev.Bytes)
	}
	out.Log(schemify.LevelInfo, t.Text())
}

// parseInt parses one decimal integer value; false on anything else.
func parseInt(vs *schemify.Values) (int64, bool) {
	v, ok := vs.Next(numBuf[:])
	i := 0
	if ok && len(v) > 0 && v[0] == '-' {
		i = 1
	}
	if !ok || len(v) <= i {
		return 0, false
	}
	var x int64
	for _, c := range v[i:] {
		if c < '0' || c > '9' || x > 1e12 {
			return 0, false
		}
		x = x*10 + int64(c-'0')
	}
	if i == 1 {
		x = -x
	}
	return x, true
}

func call(fnID uint32, args string, result *schemify.Out) schemify.Code {
	vs := schemify.NewValues(args)
	switch fnID {
	case 0:
		a, okA := parseInt(&vs)
		b, okB := parseInt(&vs)
		if _, more := vs.Next(numBuf[:1]); !okA || !okB || more || vs.Err() != nil {
			return schemify.EParse
		}
		sum := a + b
		if sum < 0 {
			result.Put("-")
			sum = -sum
		}
		d := &numBuf
		k := len(d)
		for {
			k--
			d[k] = byte('0' + sum%10)
			if sum /= 10; sum == 0 {
				break
			}
		}
		result.PutBytes(d[k:])
		return 0
	case 1:
		v, ok := vs.Next(value[:1024])
		if !ok {
			return schemify.EParse
		}
		greet := schemify.NewOut(greetBuf[:])
		greet.Put("hi ")
		greet.PutBytes(v)
		result.PutValue(greet.Text())
		return 0
	}
	return schemify.ERange
}
