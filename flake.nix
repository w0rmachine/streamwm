{
  description = "streamwm — a tiling window manager for the river Wayland compositor";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # The overlay is applied to *our* nixpkgs below, so rust-overlay's own
    # nixpkgs input is never used -- but without `follows` it still pins a
    # second, ancient nixpkgs into every consumer's lock file. Make it follow
    # so the whole graph shares one nixpkgs.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Rust toolchain pinned via rust-overlay (use a recent stable).
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" ];
        };

        # Single source of truth: the Cargo manifest. Bumping it is all a
        # release needs; the flake used to drift (0.5.2 vs 0.5.4).
        streamwmVersion = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

        # Build streamwm from the local source tree.
        streamwm = pkgs.rustPlatform.buildRustPackage {
          pname = "streamwm";
          version = streamwmVersion;
          src = self;
          cargoLock = { lockFile = ./Cargo.lock; };
          nativeBuildInputs = with pkgs; [
            rust # ensure wayland-scanner proc macros resolve
            pkg-config
            wayland-scanner
          ];
          buildInputs = with pkgs; [
            wayland
            wayland-protocols # cursor-shape-v1 (staging) for the WM pointer
            # zbus needs no system libs, but wayland-backend may link libwayland
          ];
          # Keep the dependency tree fresh; nixpkgs will fetch from crates.io.
          meta = with pkgs.lib; {
            description = "A tiling window manager for river";
            license = licenses.mit;
            mainProgram = "streamwm";
          };
        };
      in
      {
        packages.default = streamwm;
        packages.streamwm = streamwm;

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rust
            cargo
            pkg-config
            wayland
            wayland-scanner
          ];
          RUST_BACKTRACE = "1";
        };

        checks = {
          inherit streamwm;
        };
      }
    );
}
