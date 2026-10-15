{
  description = "Schemify: schematic capture for analog/mixed-signal IC design (xschem replacement)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # plugins/cktimg/vendor needs Zig 0.17 (build.zig.zon), which nixpkgs doesn't ship yet.
    zig-overlay.url = "github:mitchellh/zig-overlay";
    zig-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      zig-overlay,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # winit and wgpu `dlopen` these at runtime (Linux): Wayland first, X11 fallback;
      # fontconfig for the system-font fallback of glyphs outside the embedded UI font.
      runtimeLibs =
        pkgs:
        pkgs.lib.optionals pkgs.stdenv.isLinux (
          with pkgs;
          [
            fontconfig
            wayland
            libxkbcommon
            vulkan-loader
            libGL
            xorg.libX11
            xorg.libXcursor
            xorg.libXi
            xorg.libXrandr
          ]
        );

      # Simulators the runner spawns (D15). ngspice is the default; the others are
      # picked up when present (Xyce dialect, Verilog cosim via d_cosim).
      simulators =
        pkgs: with pkgs; [
          ngspice
          xyce
          iverilog
        ];

      # The wasm-bindgen CLI must equal the `wasm-bindgen` crate version in Cargo.lock
      # (web build, D2); nixpkgs' default CLI lags, so build the matching one.
      wasmBindgenCli =
        pkgs:
        pkgs.buildWasmBindgenCli rec {
          src = pkgs.fetchCrate {
            pname = "wasm-bindgen-cli";
            version = "0.2.129";
            hash = "sha256-pcecKQd7E8Opw6bkFoE569epUi7gh5qpQF1e5PJY6V8=";
          };
          cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
            inherit src;
            inherit (src) pname version;
            hash = "sha256-vmUrWVU7kPJJxO5qIVeAkwQyWDELO1Z4Z5gitz2kco8=";
          };
        };

      schemify =
        pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "schemify";
          version = "0.1.0";
          # Tracked files only (git), so the gitignored corpus cache never enters the store.
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          # D39: the headless core `schemify` (CLI, config, package manager) and the editor
          # `schemify-gui` (`schemify edit` runs it); plugins are prebuilt wasm assets (the
          # interop plugin's native CLI is the `interop` package below).
          cargoBuildFlags = [
            "-p"
            "schemify-cli"
            "-p"
            "schemify-app"
          ];
          # Shipped binaries carry no debug info (D39: core ≤ 2 MB, editor ≤ 10 MB; checked by
          # tools/size-check.sh, which uses this same env); the workspace release profile keeps
          # `debug = 1` for local profiling.
          env = {
            CARGO_PROFILE_RELEASE_DEBUG = "0";
            CARGO_PROFILE_RELEASE_STRIP = "symbols";
            CARGO_PROFILE_RELEASE_LTO = "fat";
            CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1";
            # No shipped code catches panics (only a corpus test does), so drop unwinding.
            CARGO_PROFILE_RELEASE_PANIC = "abort";
            # Linux (docs/dev/perf.md "Binary size"): with panic=abort and stripped symbols the unwind
            # tables only fed symbol-less backtraces (-0.5 MB editor), and packed relative
            # relocations (DT_RELR, glibc >= 2.36; nix's glibc) shrink the PIE relocation table
            # (-0.8 MB). buildRustPackage passes --target, so build scripts keep their defaults.
            RUSTFLAGS = pkgs.lib.optionalString pkgs.stdenv.isLinux "-C force-unwind-tables=no -C link-arg=-Wl,-z,pack-relative-relocs";
          };
          # The suite needs a GPU, the fetched xschem corpus and simulators; run
          # `cargo test --workspace` in the dev shell (CI does).
          doCheck = false;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          # Only the editor gets the GUI libraries. Both find the simulators; the core also runs
          # git and curl for `schemify pack` (config spec §5). The user's PATH comes first.
          postInstall = ''
            wrapProgram $out/bin/schemify \
              --suffix PATH : ${pkgs.lib.makeBinPath ((simulators pkgs) ++ [ pkgs.git pkgs.curl ])}
            wrapProgram $out/bin/schemify-gui \
              ${pkgs.lib.optionalString pkgs.stdenv.isLinux "--prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)}"} \
              --suffix PATH : ${pkgs.lib.makeBinPath (simulators pkgs)}
          '';
          meta = {
            description = "Schematic capture with simulation, docs pages and WASM plugins";
            license = pkgs.lib.licenses.mit;
            mainProgram = "schemify-gui";
            platforms = systems;
          };
        };

      # The interop plugin's native CLI (`schemify-interop xschem-import …`), from the plugins
      # workspace (D24 as amended 2026-10-12: interop is a plugin, never part of the core).
      interop =
        pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "schemify-interop";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./plugins/Cargo.lock;
          buildAndTestSubdir = "plugins";
          cargoBuildFlags = [
            "-p"
            "interop"
            "--features"
            "cli"
            "--bin"
            "schemify-interop"
          ];
          doCheck = false;
          meta = {
            description = "Schemify interop plugin, native CLI (xschem import)";
            license = pkgs.lib.licenses.mit;
            mainProgram = "schemify-interop";
            platforms = systems;
          };
        };
    in
    {
      packages = forAll (pkgs: {
        default = schemify pkgs;
        schemify = schemify pkgs;
        interop = interop pkgs;
        wasm-bindgen-cli = wasmBindgenCli pkgs;
      });

      apps = forAll (pkgs: {
        # `nix run` starts the editor; `nix run .#schemify -- netlist …` the headless core.
        default = {
          type = "app";
          program = "${schemify pkgs}/bin/schemify-gui";
        };
        schemify = {
          type = "app";
          program = "${schemify pkgs}/bin/schemify";
        };
        interop = {
          type = "app";
          program = "${interop pkgs}/bin/schemify-interop";
        };
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages =
            (with pkgs; [
              # Rust (nixpkgs' rustc ships the wasm32-unknown-unknown std).
              rustc
              cargo
              clippy
              rustfmt
              rust-analyzer
              # Plugins: wasm-ld (the nix rustc has no rust-lld); web glue.
              lld
              # plugins/cktimg/vendor (the cktimg plugin's layout engine, a Zig library with a C ABI).
              zig-overlay.packages.${pkgs.stdenv.hostPlatform.system}."0.17.0"
              (wasmBindgenCli pkgs)
              # Golden corpus (tests/corpus): xschem to make goldens, tclsh for PDK generators.
              xschem
              tcl
              # Headless GUI runs and screenshots (docs/dev/contributing.md).
              cage
              grim
              wtype
              # Misc.
              socat
              nixfmt
              # Verilog-A → OSDI for the runner's compile step (run spec §5); dev shell only,
              # not wrapped into the package (its LLVM closure is large).
              openvaf
            ])
            ++ simulators pkgs;
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
          shellHook = ''
            export LD_LIBRARY_PATH="$LD_LIBRARY_PATH''${LD_LIBRARY_PATH:+:}/run/opengl-driver/lib"
          '';
        };
      });

      formatter = forAll (pkgs: pkgs.nixfmt);
    };
}
