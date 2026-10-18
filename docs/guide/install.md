# Installing Schemify

Schemify ships as two programs:

| Program | What it is |
|---|---|
| `schemify` | The headless core: `netlist`, `check`, `run`, `fmt`, `diff`, `pack` and the other commands of [cli.md](cli.md). No window, no GPU. |
| `schemify-gui` | The editor. `schemify edit` starts it for you. |

`schemify edit` looks for `schemify-gui` in the same directory as the `schemify` binary first, then on your `PATH`. Keep the two side by side and both ways of starting the editor work. You can also run `schemify-gui` directly: it takes the same arguments (`schemify-gui path/to/cell.shcomp`).

A third program, `schemify-interop`, imports xschem libraries and PDKs. It belongs to the `interop` plugin, not the core, and lives in its own repository, [Schemify-Interop](https://github.com/OmarSiwy/Schemify-Interop):

```sh
cargo install --git https://github.com/OmarSiwy/Schemify-Interop --features cli --bin schemify-interop
```

## With Nix

The repository is a Nix flake. You need Nix with flakes enabled; no checkout is required.

```sh
nix run github:OmarSiwy/Schemify -- path/to/cell.shcomp                      # the editor
nix run github:OmarSiwy/Schemify#schemify -- netlist path/to/tb.shbench      # the core
```

Plain `nix run` starts `schemify-gui`; `#schemify` runs the core. Inside a checkout the same apps are `nix run .` and `nix run .#schemify`.

The Nix package wraps both programs so they work without further setup:

- `schemify-gui` gets the Linux graphics, input and font libraries on `LD_LIBRARY_PATH`.
- Both get ngspice, Xyce and Icarus Verilog (`iverilog`) on `PATH`, after your own `PATH`, so a simulator you installed yourself wins.
- `schemify` also gets `git` and `curl`, which `schemify pack` uses to fetch plugins.

To keep the programs installed, use `nix profile install github:OmarSiwy/Schemify`.

OpenVAF is not part of the package (its toolchain is large). Install it yourself if your designs contain Verilog-A and you simulate with ngspice (see [External programs](#external-programs)).

### Development shell

`nix develop` in a checkout gives you everything needed to build and test Schemify: the Rust toolchain, `ngspice`, `xyce`, `iverilog`, `openvaf`, `xschem` and the Linux runtime libraries, with `LD_LIBRARY_PATH` already set (including `/run/opengl-driver/lib` on NixOS). Inside it, the `cargo` commands below work as written.

## From source

You need Rust 1.95 or newer (`rustup update stable`). No system libraries are needed at build time: the editor opens the windowing and graphics libraries when it starts.

```sh
git clone https://github.com/OmarSiwy/Schemify
cd Schemify
cargo build --release -p schemify-cli -p schemify-app
```

This puts `schemify` and `schemify-gui` next to each other in `target/release/`, so `target/release/schemify edit` finds the editor. To install them, copy both into one directory on your `PATH`.

For a quick try without installing, `cargo run` builds and starts one program at a time:

| Command | Builds and runs |
|---|---|
| `cargo run --release -p schemify-app -- <cell-or-dir>` | the editor, `schemify-gui` |
| `cargo run --release -p schemify-cli -- <command> ...` | the core, `schemify` |

For example, from the checkout:

```sh
cargo run --release -p schemify-app -- crates/app/tests/fixtures/ota/ota.shcomp
cargo run --release -p schemify-cli -- netlist crates/app/tests/fixtures/rc/tb.shbench
cargo run --release -p schemify-cli -- run crates/app/tests/fixtures/rc/tb.shbench    # needs ngspice
```

`cargo run -p schemify-cli -- edit ...` only works after `schemify-gui` has been built into the same `target/` directory (or is on your `PATH`); otherwise it stops with `cannot run schemify-gui`.

The interop command line builds from its submodule (`git submodule update --init --checkout examples/plugins/interop`):

```sh
cargo run --release --manifest-path examples/plugins/interop/Cargo.toml --features cli -- \
    xschem-import ~/xschem/my_lib -o my_lib
```

## Release archives

Tagging a version (`v*`) builds archives for Linux x86_64, macOS (Apple silicon and Intel) and Windows x86_64, plus a web bundle, and attaches them to a GitHub release. No release has been published yet, so for now use Nix or build from source.

Each native archive (`schemify-<version>-<platform>.tar.gz`, `.zip` on Windows) holds `schemify` and `schemify-gui` in one directory. Unpack it and put that directory on your `PATH`. The Linux binaries need glibc 2.28 or newer (RHEL/Alma/Rocky 8, Debian 10, Ubuntu 18.10 and later); musl-based distributions are not supported.

## Runtime libraries on Linux

The editor loads these libraries when it starts, so they must be installed on the machine that runs it (the core, `schemify`, needs none of them):

| For | Libraries |
|---|---|
| Wayland sessions | `libwayland-client`, `libxkbcommon` |
| X11 sessions (used when Wayland is not available) | `libX11`, `libXcursor`, `libXi`, `libXrandr` |
| Drawing | a Vulkan driver and the Vulkan loader (`libvulkan`), or OpenGL ES through `libGL`/EGL |
| Fonts outside the built-in UI font | `fontconfig` |

On a typical desktop distribution these are already present. If the editor fails to open a window, the Vulkan loader or your GPU's Vulkan driver is the usual missing piece.

On macOS and Windows nothing extra is needed: the editor draws with Metal and Direct3D 12.

### NixOS without the Nix package

A binary you built with `cargo` outside `nix develop` cannot find the libraries above on NixOS, because they are not in a global library directory. Either run it inside `nix develop`, or point `LD_LIBRARY_PATH` at the libraries and the system GPU drivers yourself:

```sh
libs=$(nix build --no-link --print-out-paths nixpkgs#wayland nixpkgs#libxkbcommon nixpkgs#vulkan-loader \
       | sed 's|$|/lib|' | paste -sd:)
export LD_LIBRARY_PATH="$libs:/run/opengl-driver/lib"
target/release/schemify-gui crates/app/tests/fixtures/inv/top.shcomp
```

The Nix package (`nix run`, `nix profile install`) does this for you.

## External programs

Schemify writes netlists by itself. It starts other programs only to simulate or to compile device models, and it finds each one on your `PATH` under the name below unless your config names another path with `schemify.sim(...)` ([config.md](config.md)).

| Program | Needed for | Default name |
|---|---|---|
| ngspice | Simulate (`s`) and `schemify run`; the default simulator | `ngspice` |
| ESPice | Simulate and `schemify run` when you choose ESPice (`--dialect espice`, the *Netlist dialect* menu, or `schemify.opt.simulator = "espice"`). It compiles Verilog-A itself with Zig 0.17, found through the `ZIG` environment variable or `zig` on `PATH`. Not packaged with Schemify. | `espice` |
| OpenVAF | Verilog-A devices simulated with ngspice: each source is compiled to an OSDI file before the run. OpenVAF-Reloaded installs as `openvaf-r`; name it in your config. | `openvaf` |
| Icarus Verilog | Verilog devices simulated with ngspice (co-simulation through ngspice's `d_cosim`) | `iverilog` |
| Xyce | Nothing inside Schemify: `schemify run` and Simulate start only ngspice or ESPice. `schemify netlist --dialect xyce` writes a Xyce netlist you can run yourself. | |
| git, curl | `schemify pack` fetching plugins | `git`, `curl` |

A design that uses only SPICE models needs ngspice and nothing else. Netlisting, checking, formatting and diffing need no external program at all.

A minimal config that points at non-default locations:

```lua
schemify.sim("ngspice", { path = "/opt/ngspice/bin/ngspice" })
schemify.sim("openvaf", { path = "openvaf-r" })
schemify.sim("espice", { path = "/opt/espice/bin/espice", env = { ZIG = "/opt/zig-0.17/bin/zig" } })
```

## In a browser

The editor also builds for the web and runs from a static page, with files kept in browser storage; see [web.md](web.md).

See also: [tutorial.md](tutorial.md), [cli.md](cli.md), [config.md](config.md).
