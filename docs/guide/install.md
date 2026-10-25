# Installing Schemify

Schemify ships as two programs:

| Program | What it is |
|---|---|
| `schemify` | The headless core: `netlist`, `check`, `run`, `fmt`, `diff`, `pack` and the other commands of [cli.md](cli.md). No window, no GPU. |
| `schemify-gui` | The editor. `schemify edit` starts it for you. |

Simulation runs an external simulator. [ESPice](https://github.com/OmarSiwy/ESPice) is the default when it is installed, else the first one found (ngspice, Xyce, LTspice, Spectre); install it from its own repository (see [External programs](#external-programs)).

`schemify edit` looks for `schemify-gui` in the same directory as the `schemify` binary first, then on your `PATH`. Keep the two side by side and both ways of starting the editor work. You can also run `schemify-gui` directly: it takes the same arguments (`schemify-gui path/to/cell.shcomp`).

A third program, `schemify-interop`, imports xschem libraries and PDKs. It belongs to the `interop` plugin, not the core, and lives in its own repository, [Schemify-Interop](https://github.com/OmarSiwy/Schemify-Interop):

```sh
cargo install --git https://github.com/OmarSiwy/Schemify-Interop --features cli --bin schemify-interop
```

## Linux packages

Each release attaches packages built from its Linux binaries (x86_64, glibc 2.28 or newer). Download the one for your distribution from the [releases page](https://github.com/OmarSiwy/Schemify/releases) and install it:

| Distribution | Package | Install |
|---|---|---|
| Debian, Ubuntu and derivatives | `schemify_<version>-1_amd64.deb` | `sudo apt install ./schemify_<version>-1_amd64.deb` |
| Fedora, RHEL 8+, openSUSE | `schemify-<version>-1.x86_64.rpm` | `sudo dnf install ./schemify-<version>-1.x86_64.rpm` (`zypper install` on openSUSE) |
| Arch and derivatives | `schemify-<version>-1-x86_64.pkg.tar.zst` | `sudo pacman -U schemify-<version>-1-x86_64.pkg.tar.zst` |

Each package installs `schemify` and `schemify-gui` into `/usr/bin`. The editor's [runtime libraries](#runtime-libraries-on-linux) are recommended (Debian/Fedora) or optional (Arch) dependencies, because the core needs none of them.

On Arch the same binaries are also the AUR package `schemify-bin` (`yay -S schemify-bin`, or `makepkg -si` beside the `PKGBUILD` the release carries).

## With Homebrew

On macOS (Apple silicon and Intel) and on Linux x86_64:

```sh
brew install OmarSiwy/schemify/schemify
```

The formula installs the release's `schemify` and `schemify-gui`; `schemify --version` checks it.

The packages do not include a simulator: install ESPice from [its repository](https://github.com/OmarSiwy/ESPice), or your distribution's ngspice (suggested by the packages).

## With Nix

The repository is a Nix flake. You need Nix with flakes enabled; no checkout is required.

The released binaries, without compiling anything:

```sh
nix run 'github:OmarSiwy/Schemify#latest' -- path/to/cell.shcomp         # the newest release
nix run 'github:OmarSiwy/Schemify#"1.0.0"' -- path/to/cell.shcomp        # one version
nix profile install 'github:OmarSiwy/Schemify#latest'
```

These come from `sources.json` in the repository, which lists every published release's archives with their checksums (updated automatically after a release is published), for x86_64 Linux and both macOS architectures.

Built from source, at the commit you name:

```sh
nix run github:OmarSiwy/Schemify -- path/to/cell.shcomp                      # the editor
nix run github:OmarSiwy/Schemify#schemify -- netlist path/to/tb.shbench      # the core
```

Plain `nix run` starts `schemify-gui`; `#schemify` runs the core. Inside a checkout the same apps are `nix run .` and `nix run .#schemify`.

The Nix packages wrap both programs so they work without further setup:

- `schemify-gui` gets the Linux graphics, input and font libraries on `LD_LIBRARY_PATH`.
- `schemify` also gets `git` and `curl`, which `schemify pack` uses to fetch plugins.

To keep the programs installed, use `nix profile install github:OmarSiwy/Schemify` (from source) or the `#latest` line above.

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
cargo run --release -p schemify-app -- crates/view/app/tests/fixtures/ota/ota.shcomp
cargo run --release -p schemify-cli -- netlist crates/view/app/tests/fixtures/rc/tb.shbench
cargo run --release -p schemify-cli -- run crates/view/app/tests/fixtures/rc/tb.shbench    # needs ngspice
```

`cargo run -p schemify-cli -- edit ...` only works after `schemify-gui` has been built into the same `target/` directory (or is on your `PATH`); otherwise it stops with `cannot run schemify-gui`.

The interop command line builds from its submodule (`git submodule update --init --checkout examples/plugins/interop`):

```sh
cargo run --release --manifest-path examples/plugins/interop/Cargo.toml --features cli -- \
    xschem-import ~/xschem/my_lib -o my_lib
```

## Release archives

Every release (`publish.yaml`, on a `v*` tag) attaches archives for Linux x86_64, macOS (Apple silicon and Intel) and Windows x86_64, the Linux packages above, a web bundle and `SHA256SUMS`.

Each native archive (`schemify-v<version>-<platform>.tar.gz`, `.zip` on Windows) holds `schemify` and `schemify-gui` in one directory. Unpack it and put that directory on your `PATH`. The Linux binaries need glibc 2.28 or newer (RHEL/Alma/Rocky 8, Debian 10, Ubuntu 18.10 and later); musl-based distributions are not supported.

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
target/release/schemify-gui crates/view/app/tests/fixtures/inv/top.shcomp
```

The Nix package (`nix run`, `nix profile install`) does this for you.

## External programs

Schemify writes netlists by itself. It starts other programs only to simulate or to compile device models, and it finds each one on your `PATH` under the name below unless your config names another path with `schemify.sim(...)` ([config.md](config.md)).

| Program | Needed for | Default name |
|---|---|---|
| ESPice | Simulate (`s`) and `schemify run`; the default simulator when installed (not packaged with Schemify; install it from [its repository](https://github.com/OmarSiwy/ESPice)). It compiles Verilog-A itself with Zig 0.17, found through the `ZIG` environment variable or `zig` on `PATH`. | `espice` |
| ngspice | Simulate and `schemify run` when you choose ngspice (`--dialect ngspice`, the *Netlist dialect* menu, or `schemify.opt.simulator = "ngspice"`), or when it is installed and ESPice is not | `ngspice` |
| OpenVAF | Verilog-A devices simulated with ngspice: each source is compiled to an OSDI file before the run. OpenVAF-Reloaded installs as `openvaf-r`; name it in your config. | `openvaf` |
| Icarus Verilog | Verilog devices simulated with ngspice (co-simulation through ngspice's `d_cosim`) | `iverilog` |
| Xyce | Nothing inside Schemify: `schemify run` and Simulate start only ngspice or ESPice. `schemify netlist --dialect xyce` writes a Xyce netlist you can run yourself. | |
| git, curl | `schemify pack` fetching plugins | `git`, `curl` |

A design that uses only SPICE models needs ESPice (or ngspice) and nothing else. Netlisting, checking, formatting and diffing need no external program at all.

A minimal config that points at non-default locations:

```lua
schemify.sim("ngspice", { path = "/opt/ngspice/bin/ngspice" })
schemify.sim("openvaf", { path = "openvaf-r" })
schemify.sim("espice", { path = "/opt/espice/bin/espice", env = { ZIG = "/opt/zig-0.17/bin/zig" } })
```

## In a browser

The editor also builds for the web and runs from a static page, with files kept in browser storage; see [web.md](web.md).

See also: [tutorial.md](tutorial.md), [cli.md](cli.md), [config.md](config.md).
