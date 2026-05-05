{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-parts.url = "github:hercules-ci/flake-parts";
    devenv.url = "github:cachix/devenv/v1.6.1";
    pre-commit-hooks.url = "github:cachix/git-hooks.nix";
    v_flakes.url = "github:valeratrades/v_flakes?ref=v1.6";
  };

  outputs = inputs@{ self, nixpkgs, rust-overlay, flake-parts, devenv, pre-commit-hooks, v_flakes }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        devenv.flakeModule
      ];

      systems = nixpkgs.lib.systems.flakeExposed;

      perSystem = { config, self', inputs', system, ... }:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
            config.allowUnfree = true;
          };
          rust = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {
            extensions = [ "rust-src" "rust-analyzer" "rust-docs" "rustc-codegen-cranelift-preview" ];
          });
          pre-commit-check = pre-commit-hooks.lib.${system}.run (v_flakes.files.preCommit { inherit pkgs; });
          manifest = (pkgs.lib.importTOML ./Cargo.toml).package;
          pname = manifest.name;
          stdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.stdenv;
          python = pkgs.python312;

          rs = v_flakes.rs { inherit pkgs rust; };
          py = v_flakes.py { inherit pkgs; };
          github = v_flakes.github {
            inherit pkgs pname rs py;
            enable = true;
            lastSupportedVersion = "nightly-2025-10-10";
            jobs = {
              errors.replace = [ "rust-tests" ];
              warnings.replace = [ "rust-doc" "rust-clippy" "rust-machete" "rust-sorted" "tokei" ];
              other.replace = [ "loc-badge" ];
            };
          };
          readme = v_flakes.readme-fw {
            inherit pkgs pname;
            lastSupportedVersion = "nightly-1.92";
            rootDir = ./.;
            licenses = [{ license = v_flakes.files.licenses.blue_oak; }];
            badges = [ "msrv" "crates_io" "docs_rs" "loc" "ci" ];
          };
          combined = v_flakes.utils.combine [ rs py github readme ];

          # Native libs that prebuilt Python wheels (numpy, torch, kokoro deps) dlopen at runtime.
          pyRuntimeLibs = with pkgs; [
            stdenv.cc.cc.lib # libstdc++.so.6, libgcc_s, libgomp
            zlib
          ];

          rustPlatform = pkgs.makeRustPlatform {
            rustc = rust;
            cargo = rust;
            inherit stdenv;
          };
        in
        {
          _module.args.pkgs = pkgs;

          packages.default = rustPlatform.buildRustPackage {
            inherit pname;
            version = manifest.version;

            nativeBuildInputs = with pkgs; [ pkg-config ];

            cargoLock.lockFile = ./Cargo.lock;
            src = pkgs.lib.cleanSource ./.;
          };

          devenv.shells.default = {
            languages.python = {
              enable = true;
              package = python;
              uv = {
                enable = true;
                sync.enable = false;
              };
            };

            packages = [
              pkgs.mold
              pkgs.pkg-config
              rust
            ] ++ pyRuntimeLibs ++ pre-commit-check.enabledPackages ++ combined.enabledPackages;

            env = {
              RUST_BACKTRACE = 1;
              RUST_LIB_BACKTRACE = 0;
            };

            enterShell =
              pre-commit-check.shellHook
              + combined.shellHook
              + ''
                export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath pyRuntimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
              '';
          };
        };
    };
}
