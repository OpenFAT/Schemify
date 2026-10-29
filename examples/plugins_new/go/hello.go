// A Schemify plugin in Go: Tools → Count nets logs the nets with a single connection.
// Build with TinyGo (standard Go's wasm imports WASI): see go.mod.
package main

import schemify "github.com/OmarSiwy/Schemify/crates/protocol/abi/sdk/go"

// Static buffers: TinyGo's wasm-unknown heap never frees, so don't allocate per event.
var (
	pins            [1 << 16]uint32
	names           [1 << 20]byte
	listBuf, msgBuf [1100]byte
)

func init() { schemify.Handle(onEvent) }

func main() {} // never called; init registers the handler

func onEvent(ev *schemify.Event, out *schemify.Out) {
	switch ev.Kind {
	case schemify.EventLoad, schemify.EventDocChanged:
		// Contributions are immediate-mode: send the whole set every time.
		out.Contribute(schemify.KindUi, `node tools.hello item
  parent menu.tools
  label "Count nets"
  command hello.count`)
		out.Contribute(schemify.KindCommands, `command hello.count "Count nets"`)
	case schemify.EventCommand:
		if ev.Name == "hello.count" {
			countNets(out)
		}
	}
}

func countNets(out *schemify.Out) {
	n, _ := schemify.TableLen(schemify.Nets)
	n = min(n, len(pins))
	if _, err := schemify.ReadColumn(schemify.Nets, schemify.NetsPinCount, 0, pins[:n]); err != nil {
		n = 0
	}
	strs, _ := schemify.ReadStrings(schemify.Nets, schemify.NetsName, 0, uint32(n), names[:])
	list, msg := schemify.NewOut(listBuf[:]), schemify.NewOut(msgBuf[:])
	single := uint32(0)
	for i := 0; i < n; i++ {
		name, ok := strs.Next()
		if !ok {
			break
		}
		if pins[i] != 1 {
			continue
		}
		if single == 0 {
			list.Put(": ")
		} else {
			list.Put(", ")
		}
		list.Put(name)
		single++
	}
	msg.PutU32(single)
	msg.Put(" single-connection nets")
	msg.Put(list.Text())
	out.Log(schemify.LevelInfo, msg.Text())
}
