{
  description = "rhonometre Rust/Leptos water dashboard";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
      rustToolchainFor = pkgs:
        pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "clippy" "rustfmt" ];
          targets = [ "wasm32-unknown-unknown" ];
        };
      runnerFor = system:
        let
          pkgs = pkgsFor system;
          rustToolchain = rustToolchainFor pkgs;
        in
        pkgs.writeShellApplication {
          name = "rhonometre";
          runtimeInputs = [
            rustToolchain
            pkgs.pkg-config
            pkgs.openssl
            pkgs.trunk
            pkgs.wasm-bindgen-cli
          ];
          text = ''
            if [ ! -f Cargo.toml ] || [ ! -f frontend/index.html ]; then
              echo "Run this command from the rhonometre repository root." >&2
              exit 1
            fi

            export RUST_BACKTRACE="''${RUST_BACKTRACE:-1}"
            export STATIC_DIR="frontend/dist"
            unset NO_COLOR

            echo "Building Leptos frontend into $STATIC_DIR"
            (
              cd frontend
              trunk build index.html --dist dist
            )

            echo "Starting rhonometre at http://127.0.0.1:''${PORT:-3000}"
            cargo run -p nivrhone-server
          '';
        };
    in
    {
      apps = forAllSystems (system:
        {
          default = {
            type = "app";
            program = "${runnerFor system}/bin/rhonometre";
          };
        });

      devShells = forAllSystems (system:
        let
          pkgs = pkgsFor system;
          rustToolchain = rustToolchainFor pkgs;
        in
        {
          default = pkgs.mkShell {
            packages = [
              rustToolchain
              pkgs.pkg-config
              pkgs.openssl
              pkgs.trunk
              pkgs.wasm-bindgen-cli
            ];

            RUST_BACKTRACE = "1";
          };
        });
    };
}
