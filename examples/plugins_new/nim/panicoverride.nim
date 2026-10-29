# Required by --os:standalone: where Nim sends fatal errors. A wasm trap is the honest answer.
proc rawoutput(s: string) = discard
proc panic(s: string) {.noreturn.} =
  {.emit: "__builtin_trap();".}
  while true: discard
