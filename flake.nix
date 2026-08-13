{
  description = "streamwm — a tiling window manager for the river Wayland compositor";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # Match the dotfiles' pinned nixpkgs (nixos-26.05) for ABI compatibility
    # when consumed as a home-manager input.
    rust-overlay.url = "github:oxalica/rust-overlay";
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

        # Build streamwm from the local source tree.
        streamwm = pkgs.rustPlatform.buildRustPackage {
          pname = "streamwm";
          version = "0.1.0";
          src = self;
          cargoLock = { lockFile = ./Cargo.lock; };
          nativeBuildInputs = with pkgs; [
            rust # ensure wayland-scanner proc macros resolve
            pkg-config
          ];
          buildInputs = with pkgs; [
            wayland
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
