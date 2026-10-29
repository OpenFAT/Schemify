// Build: tinygo build -target=wasm-unknown -no-debug -o hello.wasm .
module hello

go 1.23

require github.com/OmarSiwy/Schemify/crates/protocol/abi/sdk/go v0.0.0-00010101000000-000000000000

// Schemify's Go library, in-repo. In your own repository delete this line and pin a
// Schemify revision: go get github.com/OmarSiwy/Schemify/crates/protocol/abi/sdk/go@<commit>
replace github.com/OmarSiwy/Schemify/crates/protocol/abi/sdk/go => ../../../crates/protocol/abi/sdk/go
