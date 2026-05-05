{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/b3d51a0365f6695e7dd5cdf3e180604530ed33b4";
    rust-overlay.url = "github:oxalica/rust-overlay/3a0ebe5d2965692f990cb27e62f501ad35e3deeb";
    flake-utils.url = "github:numtide/flake-utils/11707dc2f618dd54ca8739b309ec4fc024de578b";
    v_flakes.url = "github:valeratrades/v_flakes/257142a54b071bb8a8b2e031d69e70f416518a5f";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, v_flakes }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
          allowUnfree = true;
        };
        rust = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {
          extensions = [ "rust-src" "rust-analyzer" "rust-docs" "rustc-codegen-cranelift-preview" ];
        });

        manifest = (pkgs.lib.importTOML ./Cargo.toml).package;
        pname = manifest.name;
        stdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.stdenv;

        rs = v_flakes.rs { inherit pkgs rust; };
        github = v_flakes.github {
          inherit pkgs pname rs;
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
        combined = v_flakes.utils.combine [ rs github readme ];
      in
      {
        packages =
          let
            rustc = rust;
            cargo = rust;
            rustPlatform = pkgs.makeRustPlatform {
              inherit rustc cargo stdenv;
            };
          in
          {
            default = rustPlatform.buildRustPackage rec {
              inherit pname;
              version = manifest.version;

              nativeBuildInputs = with pkgs; [ pkg-config ];

              cargoLock.lockFile = ./Cargo.lock;
              src = pkgs.lib.cleanSource ./.;
            };
          };

        devShells.default =
          with pkgs;
          mkShell {
            inherit stdenv;
            shellHook = combined.shellHook;

            env = {
              RUST_BACKTRACE = 1;
              RUST_LIB_BACKTRACE = 0;
            };

            packages = [
              mold
              pkg-config
              rust
            ] ++ combined.enabledPackages;
          };
      }
    );
}
