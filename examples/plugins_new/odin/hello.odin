// A Schemify plugin in Odin: Tools → Count nets logs the nets with a single connection.
//
// Build (freestanding wasm32, no WASI, no entry point; docs/plugins/languages.md):
//
//   odin build . -target:freestanding_wasm32 -no-entry-point -o:size -out:hello.wasm \
//     -collection:schemify=../../../crates/protocol/abi/sdk/odin
//
// The collection points at Schemify's Odin library (crates/protocol/abi/sdk/odin). In your
// own repository, vendor that directory from a pinned Schemify revision (or a submodule)
// and point -collection:schemify at it.
package hello

import "schemify:schemify"

// The host calls exports without an Odin context, so every proc here is "contextless".
@(export)
schemify_on_event :: proc "contextless" (ptr, n: u32) -> u64 {
	return schemify.on_event(ptr, n, on_event)
}

pins: [1 << 16]u32
names: [1 << 20]u8

on_event :: proc "contextless" (ev: schemify.Event, out: ^schemify.Out) {
	#partial switch ev.kind {
	case .Load, .Doc_Changed:
		// Contributions are immediate-mode: send the whole set every time.
		schemify.contribute(out, .Ui, "node tools.hello item\n" +
			"  parent menu.tools\n" +
			"  label \"Count nets\"\n" +
			"  command hello.count\n")
		schemify.contribute(out, .Commands, "command hello.count \"Count nets\"\n")
	case .Command:
		if ev.name == "hello.count" do count_nets(out)
	}
}

count_nets :: proc "contextless" (out: ^schemify.Out) {
	n := schemify.table_len(.Nets)
	if n < 0 || int(n) > len(pins) do n = 0
	if schemify.read_column(.Nets, u32(schemify.Nets_Col.Pin_Count), 0, u32(n), pins[:n]) < 0 do n = 0
	got := schemify.read_strings(.Nets, u32(schemify.Nets_Col.Name), 0, u32(n), names[:])
	rest := names[:max(got, 0)]

	list_buf, msg_buf: [1024]u8
	list := schemify.Out{buf = list_buf[:]}
	msg := schemify.Out{buf = msg_buf[:]}
	single: u32
	for i in 0 ..< n {
		name := schemify.strings_next(&rest) or_break
		if pins[i] != 1 do continue
		schemify.put(&list, single == 0 ? ": " : ", ")
		schemify.put(&list, name)
		single += 1
	}
	schemify.put_u32(&msg, single)
	schemify.put(&msg, " single-connection nets")
	schemify.put(&msg, schemify.out_str(&list))
	schemify.log(out, .Info, schemify.out_str(&msg))
}
