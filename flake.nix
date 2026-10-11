{
  description = "Schemify: schematic capture for analog/mixed-signal IC design (xschem replacement)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # extern/cktimg needs Zig 0.17 (build.zig.zon), which nixpkgs doesn't ship yet.
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

      # winit and wgpu `dlopen` these at runtime (Linux): Wayland first, X11 fallback.
      runtimeLibs =
        pkgs:
        pkgs.lib.optionals pkgs.stdenv.isLinux (
          with pkgs;
          [
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
          # The editor/CLI and the standalone importer; plugins are prebuilt wasm assets.
          cargoBuildFlags = [
            "-p"
            "schemify-app"
            "-p"
            "schemify-devicereader"
          ];
          # Shipped binaries carry no debug info (D39: native binary ≤ 25 MB); the
          # workspace release profile keeps `debug = 1` for local profiling.
          env = {
            CARGO_PROFILE_RELEASE_DEBUG = "0";
            CARGO_PROFILE_RELEASE_STRIP = "symbols";
            CARGO_PROFILE_RELEASE_LTO = "fat";
            CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1";
            # No shipped code catches panics (only a corpus test does), so drop unwinding.
            CARGO_PROFILE_RELEASE_PANIC = "abort";
          };
          # The suite needs a GPU, the fetched xschem corpus and simulators; run
          # `cargo test --workspace` in the dev shell (CI does).
          doCheck = false;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postInstall = pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            wrapProgram $out/bin/schemify \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)} \
              --suffix PATH : ${pkgs.lib.makeBinPath (simulators pkgs)}
          '';
          meta = {
            description = "Schematic capture with simulation, docs pages and WASM plugins";
            license = with pkgs.lib.licenses; [
              mit
              asl20
            ];
            mainProgram = "schemify";
            platforms = systems;
          };
        };
    in
    {
      packages = forAll (pkgs: {
        default = schemify pkgs;
        schemify = schemify pkgs;
        wasm-bindgen-cli = wasmBindgenCli pkgs;
      });

      apps = forAll (pkgs: {
        default = {
          type = "app";
          program = "${schemify pkgs}/bin/schemify";
        };
        devicereader = {
          type = "app";
          program = "${schemify pkgs}/bin/devicereader";
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
              # extern/cktimg (net2schem's layout engine, a Zig library with a C ABI).
              zig-overlay.packages.${pkgs.stdenv.hostPlatform.system}."0.17.0"
              (wasmBindgenCli pkgs)
              # Golden corpus (tests/corpus): xschem to make goldens, tclsh for PDK generators.
              xschem
              tcl
              # Headless GUI runs and screenshots (AGENTS.md).
              cage
              grim
              wtype
              # Misc.
              socat
              nixfmt
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
