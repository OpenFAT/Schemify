// The Odin library's conformance guest (../../CONFORMANCE.md). Build, in this directory:
// odin build . -target:freestanding_wasm32 -no-entry-point -o:size -out:conformance.wasm
package conformance

import sch "../schemify"

@(export)
schemify_on_event :: proc "contextless" (ptr, n: u32) -> u64 {return sch.on_event(ptr, n, handle)}
@(export)
schemify_call :: proc "contextless" (fn_id, ptr, n: u32) -> u64 {return sch.call(fn_id, ptr, n, call)}

text_buf: [1 << 16]u8

echo_args :: proc "contextless" (t: ^sch.Out, args: string) {
	v: [4096]u8
	it := sch.Values{rest = args}
	for s in sch.values_next(&it, v[:]) {
		sch.put(t, " [")
		sch.put(t, s)
		sch.put(t, "]")
	}
	if it.malformed do sch.put(t, " !parse")
}

out_records :: proc "contextless" (out: ^sch.Out) {
	sch.contribute_scoped(out, .Annotations, "erc pass", `mark R1 warn "hi"`)
	sch.edit(out, 1, true, "size it", "set_attr M1 w 2u")
	sch.edit(out, 1, false, "move", "set_attr R1 at 10 20\nset_attr R2 at 0 0")
	sch.run(out, "lib:tb", "default", "tt")
	sch.vary(out, "vdd", "1.8")
	sch.vary(out, "w", "two words")
	sch.timer(out, 7, 250)
	sch.log(out, .Warn, "q\"\\\tx\ny\x01")
	sch.log(out, .Info, "done")
}

read_nets :: proc "contextless" (out, t: ^sch.Out) {
	@(static) pins: [64]u32
	@(static) names: [4096]u8
	n := sch.table_len(.Nets)
	if n < 0 || n > 64 do n = 0
	sch.read_column(.Nets, u32(sch.Nets_Col.Pin_Count), 0, u32(n), pins[:n])
	got := sch.read_strings(.Nets, u32(sch.Nets_Col.Name), 0, u32(n), names[:])
	rest := names[:max(got, 0)]
	sch.put(t, "nets ")
	sch.put_u32(t, u32(n))
	sch.put(t, ":")
	for i in 0 ..< n {
		name := sch.strings_next(&rest) or_break
		sch.put(t, " ")
		sch.put(t, name)
		sch.put(t, "=")
		sch.put_u32(t, pins[i])
	}
	sch.log(out, .Info, sch.out_str(t))
	t.len = 0
	sch.put(t, "errors ")
	sch.put_i32(t, -sch.table_len(sch.Table(99)))
	sch.put(t, " ")
	sch.put_i32(t, -sch.read_column(.Nets, u32(sch.Nets_Col.Name), 0, u32(n), pins[:n]))
	sch.put(t, " ")
	sch.put_i32(t, -sch.read_column(.Nets, u32(sch.Nets_Col.Pin_Count), 0, u32(n + 1), pins[:n + 1]))
}

handle :: proc "contextless" (ev: sch.Event, out: ^sch.Out) {
	t := sch.Out{buf = text_buf[:]}
	switch ev.kind {
	case .Load:
		sch.contribute(out, .Commands, "command conf.echo \"Echo\"\ncommand conf.out \"Out\"\ncommand conf.read \"Read\"")
		sch.contribute(out, .Functions, "function conf.add 0 2\nfunction conf.greet 1 1")
		sch.put(&t, "load")
	case .Unload: sch.put(&t, "unload")
	case .Selection_Changed: sch.put(&t, "selection_changed")
	case .Command:
		switch ev.name {
		case "conf.out":
			out_records(out)
			return
		case "conf.read":
			read_nets(out, &t)
		case:
			sch.put(&t, "command ")
			sch.put(&t, ev.name)
			echo_args(&t, ev.args)
		}
	case .Doc_Opened: sch.put(&t, "doc_opened "); sch.put_u32(&t, ev.doc)
	case .Doc_Closed: sch.put(&t, "doc_closed "); sch.put_u32(&t, ev.doc)
	case .Saved: sch.put(&t, "saved "); sch.put_u32(&t, ev.doc)
	case .Doc_Changed:
		sch.put(&t, "doc_changed ")
		sch.put_u32(&t, ev.doc)
		sch.put(&t, " ")
		sch.put_u32(&t, ev.epoch)
	case .Netlist_Request:
		sch.put(&t, "netlist_request ")
		sch.put(&t, ev.dialect)
		sch.put(&t, " ")
		sch.put(&t, ev.bench)
	case .Run_Job:
		sch.put(&t, "run_job ")
		sch.put_u32(&t, ev.job)
		sch.put(&t, " ")
		sch.put(&t, ev.lang)
	case .Run_Done: sch.put(&t, "run_done "); sch.put_u32(&t, ev.job)
	case .Timer: sch.put(&t, "timer "); sch.put_u32(&t, ev.id)
	case .Io_Done:
		sch.put(&t, "io_done ")
		sch.put_u32(&t, ev.handle)
		sch.put(&t, " ")
		sch.put_i32(&t, ev.status)
		sch.put(&t, " ")
		sch.put(&t, ev.bytes)
	case .Rejected:
		sch.put(&t, "rejected ")
		sch.put_u32(&t, u32(ev.code))
		sch.put(&t, " ")
		sch.put(&t, ev.bytes)
	}
	sch.log(out, .Info, sch.out_str(&t))
}

// One decimal integer value; false on anything else.
parse_int :: proc "contextless" (it: ^sch.Values) -> (v: i64, ok: bool) {
	b: [24]u8
	s := sch.values_next(it, b[:]) or_return
	neg := len(s) > 0 && s[0] == '-'
	if len(s) <= int(neg) do return
	for i in int(neg) ..< len(s) {
		if s[i] < '0' || s[i] > '9' || v > 1_000_000_000_000 do return 0, false
		v = v * 10 + i64(s[i] - '0')
	}
	return neg ? -v : v, true
}

call :: proc "contextless" (fn_id: u32, args: string, result: ^sch.Out) -> sch.Code {
	it := sch.Values{rest = args}
	switch fn_id {
	case 0:
		a, ok_a := parse_int(&it)
		b, ok_b := parse_int(&it)
		extra: [1]u8
		_, more := sch.values_next(&it, extra[:])
		if !ok_a || !ok_b || more || it.malformed do return .Parse
		sum := a + b
		if sum < 0 do sch.put(result, "-")
		d: [24]u8
		k := len(d)
		for u := u64(abs(sum)); ; u /= 10 {
			k -= 1
			d[k] = '0' + u8(u % 10)
			if u < 10 do break
		}
		sch.put(result, string(d[k:]))
		return sch.OK
	case 1:
		v, g: [1024]u8
		s, ok := sch.values_next(&it, v[:])
		if !ok do return .Parse
		greet := sch.Out{buf = g[:]}
		sch.put(&greet, "hi ")
		sch.put(&greet, s)
		sch.put_value(result, sch.out_str(&greet))
		return sch.OK
	}
	return .Range
}
