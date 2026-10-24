{
  description = "Schemify: schematic capture for analog/mixed-signal IC design (xschem replacement)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # The pinned Rust toolchain (`rust-version` in Cargo.toml, CI's RUST_TOOLCHAIN): nixpkgs
    # ships only its current rustc, and the guest modules' bytes depend on the compiler.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAll =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );

      # Rust 1.95.0 with the guest targets; it ships rust-lld, which links the wasm guests.
      toolchain =
        pkgs:
        pkgs.rust-bin.stable."1.95.0".default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
          targets = [
            "wasm32-unknown-unknown"
            "wasm32-wasip1"
          ];
        };
      rustPlatform =
        pkgs:
        pkgs.makeRustPlatform {
          cargo = toolchain pkgs;
          rustc = toolchain pkgs;
        };

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
        (rustPlatform pkgs).buildRustPackage {
          pname = "schemify";
          version = "0.1.0";
          # Tracked files only (git).
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          # D39: the headless core `schemify` (CLI, config, package manager) and the editor
          # `schemify-gui` (`schemify edit` runs it); the default UI is a prebuilt wasm asset.
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
          # The suite needs a GPU and simulators; run
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
      # The web editor (docs/guide/web.md): `index.html` + the wasm-bindgen bundle in `pkg/`,
      # ready to serve, or to sit beside `schemify publish`'s `projects/` on a static site.
      web =
        pkgs:
        (rustPlatform pkgs).buildRustPackage {
          pname = "schemify-web";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ (wasmBindgenCli pkgs) ];
          auditable = false;
          doCheck = false;
          buildPhase = ''
            runHook preBuild
            cargo build --release --frozen --target wasm32-unknown-unknown -p schemify-app --bin schemify-gui
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            mkdir -p $out
            cp crates/app/web/index.html $out/
            wasm-bindgen --target web --no-typescript --out-name schemify --out-dir $out/pkg \
              target/wasm32-unknown-unknown/release/schemify-gui.wasm
            runHook postInstall
          '';
        };
    in
    {
      packages = forAll (pkgs: {
        default = schemify pkgs;
        schemify = schemify pkgs;
        web = web pkgs;
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
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages =
            (with pkgs; [
              # Rust 1.95.0: cargo, rustc, clippy, rustfmt, rust-analyzer, rust-lld (guests).
              (toolchain pkgs)
              # Web glue.
              (wasmBindgenCli pkgs)
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
