{
  inputs = {
    v_flakes.url = "github:valeratrades/v_flakes?ref=v1.6";
  };

  outputs = inputs@{ self, v_flakes }:
    let
      inherit (v_flakes) flake-parts devenv nixpkgs pre-commit-hooks;
    in
    # devenv's flakeModule evaluates `inputs.nixpkgs.lib`, so nixpkgs has to reach it as a flake.
    flake-parts.lib.mkFlake { inputs = inputs // { inherit nixpkgs; }; } {
      imports = [
        devenv.flakeModule
      ];

      systems = nixpkgs.lib.systems.flakeExposed;

      perSystem = { config, self', inputs', system, ... }:
        let
          pkgs = import v_flakes.default_nixpkgs {
            inherit system;
            config.allowUnfree = true;
          };
          rust = v_flakes.rs.default_nightly system;
          pre-commit-check = pre-commit-hooks.lib.${system}.run (v_flakes.files.preCommit { inherit pkgs; });
          manifest = (pkgs.lib.importTOML ./Cargo.toml).package;
          pname = manifest.name;
          stdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.stdenv;
          python = pkgs.python312;

          rs = v_flakes.rs {
            inherit pkgs rust;
          };
          py = v_flakes.py { inherit pkgs; };
          github = v_flakes.github {
            inherit pkgs pname rs py;
            enable = true;
            lastSupportedVersion = "nightly-${v_flakes.rs.nightly_version}";
            jobs = {
              errors.augment = [ "rust-tests" ];
              warnings.augment = [ "rust-doc" "rust-clippy" "rust-machete" "rust-sorted" "tokei" ];
              other.augment = [ "loc-badge" ];
            };
          };
          readme = v_flakes.readme-fw {
            inherit pkgs pname;
            lastSupportedVersion = "nightly-1.92";
            rootDir = ./.;
            licenses = [{ license = v_flakes.files.licenses.blue_oak; }];
            badges = [ "msrv" "crates_io" "docs_rs" "loc" "ci" ];
          };
          combined = v_flakes.utils.combine { inherit rust; modules = [ rs py github readme ]; };

          # Native libs that prebuilt Python wheels (numpy, torch, kokoro deps) dlopen at runtime.
          pyRuntimeLibs = with pkgs; [
            stdenv.cc.cc.lib # libstdc++.so.6, libgcc_s, libgomp
            zlib
          ];

          build_rust = v_flakes.rs.build_nightly system;
          rustPlatform = pkgs.makeRustPlatform {
            rustc = build_rust;
            cargo = build_rust;
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
            RUSTC_WRAPPER = ""; # .cargo/config.toml asks for sccache, which the build sandbox has not got
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
