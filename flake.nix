{
  description = "rhonometre Axum/Dioxus water dashboard";

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
        let
          iosTargets = pkgs.lib.optionals pkgs.stdenv.isDarwin [
            "aarch64-apple-ios"
            "aarch64-apple-ios-sim"
            "x86_64-apple-ios"
          ];
        in
        pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "clippy" "rustfmt" ];
          targets = [ "wasm32-unknown-unknown" ] ++ iosTargets;
        };
      rustPlatformFor = pkgs:
        let
          rustToolchain = rustToolchainFor pkgs;
        in
        pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
      packageFor = system:
        let
          pkgs = pkgsFor system;
          rustPlatform = rustPlatformFor pkgs;
        in
        rustPlatform.buildRustPackage {
          pname = "rhonometre";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [
            pkgs.makeWrapper
            pkgs.pkg-config
            pkgs.trunk
            pkgs.wasm-bindgen-cli
          ];

          buildInputs = [
            pkgs.openssl
          ];

          doCheck = false;

          buildPhase = ''
            runHook preBuild
            export HOME="$TMPDIR"
            cargo build --release -p nivrhone-server
            (
              cd frontend
              trunk build index.html --dist dist --release
            )
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            mkdir -p "$out/bin" "$out/share/rhonometre"
            cp target/release/nivrhone-server "$out/bin/rhonometre-server"
            cp -r frontend/dist "$out/share/rhonometre/dist"
            makeWrapper "$out/bin/rhonometre-server" "$out/bin/rhonometre" \
              --set STATIC_DIR "$out/share/rhonometre/dist"
            runHook postInstall
          '';
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
            pkgs.postgresql_16
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

            echo "Building Dioxus frontend into $STATIC_DIR"
            (
              cd frontend
              trunk build index.html --dist dist
            )

            echo "Starting rhonometre at http://127.0.0.1:''${PORT:-3000}"
            cargo run -p nivrhone-server
          '';
        };
      dockerBuildRunnerFor = system:
        let
          pkgs = pkgsFor system;
        in
        pkgs.writeShellApplication {
          name = "rhonometre-docker-build";
          runtimeInputs = [
            pkgs.docker
          ];
          text = ''
            if [ ! -f Dockerfile ] || [ ! -f Cargo.toml ] || [ ! -f frontend/index.html ]; then
              echo "Run this command from the rhonometre repository root." >&2
              exit 1
            fi

            image_tag="''${RHONOMETRE_DOCKER_TAG:-rhonometre:local}"
            export DOCKER_BUILDKIT="''${DOCKER_BUILDKIT:-1}"

            if ! docker info >/dev/null 2>&1; then
              echo "Docker is installed in this Nix app, but no Docker daemon is reachable." >&2
              echo "Start Docker Desktop, Colima, or your Jelastic-compatible Docker daemon, then retry:" >&2
              echo "  nix run .#docker-build" >&2
              exit 1
            fi

            echo "Building Docker image $image_tag"
            platform_arg=()
            if [ -n "''${RHONOMETRE_DOCKER_PLATFORM:-}" ]; then
              platform_arg=(--platform "$RHONOMETRE_DOCKER_PLATFORM")
            fi

            docker build --pull "''${platform_arg[@]}" --tag "$image_tag" .

            echo "Built $image_tag"
            echo "Try it locally with:"
            echo "  docker run --rm -p 8080:8080 -e DATABASE_URL=... -e RHONOMETRE_INGEST_TOKEN=... -e RHONOMETRE_PRO_CODE=... -e RHONOMETRE_TOKEN_SECRET=... $image_tag"
          '';
        };
      dockerUpRunnerFor = system:
        let
          pkgs = pkgsFor system;
        in
        pkgs.writeShellApplication {
          name = "rhonometre-docker-up";
          runtimeInputs = [
            pkgs.colima
            pkgs.docker
          ];
          text = ''
            if [ "''${1:-}" = "--check" ]; then
              colima version
              docker --version
              exit 0
            fi

            if docker info >/dev/null 2>&1; then
              echo "Docker daemon is already reachable."
              docker context show
              exit 0
            fi

            if [ "$(uname -s)" != "Darwin" ]; then
              echo "This helper starts a Colima VM-backed Docker daemon and is intended for macOS." >&2
              echo "On Linux, use the system Docker service or a rootless Docker setup, then run:" >&2
              echo "  nix run .#docker-build" >&2
              exit 1
            fi

            cpu="''${COLIMA_CPU:-2}"
            memory="''${COLIMA_MEMORY:-4}"
            disk="''${COLIMA_DISK:-20}"

            echo "Starting Colima Docker daemon with $cpu CPU, $memory GiB RAM, $disk GiB disk"
            colima start --runtime docker --cpu "$cpu" --memory "$memory" --disk "$disk"

            docker info >/dev/null
            echo "Docker daemon is ready."
            echo "Now run:"
            echo "  nix run .#docker-build"
          '';
        };
      dockerDownRunnerFor = system:
        let
          pkgs = pkgsFor system;
        in
        pkgs.writeShellApplication {
          name = "rhonometre-docker-down";
          runtimeInputs = [
            pkgs.colima
          ];
          text = ''
            colima stop
          '';
        };
      iosRunnerFor = system: device:
        let
          pkgs = pkgsFor system;
          rustToolchain = rustToolchainFor pkgs;
          defaultApiBase =
            if device then
              "http://127.0.0.1:3000"
            else
              "http://127.0.0.1:3000";
        in
        pkgs.writeShellApplication {
          name = if device then "rhonometre-ios-device" else "rhonometre-ios-simulator";
          runtimeInputs = [
            rustToolchain
            pkgs.dioxus-cli
            pkgs.pkg-config
            pkgs.openssl
          ];
          text = ''
            if [ ! -d frontend ] || [ ! -f frontend/Dioxus.toml ]; then
              echo "Run this command from the rhonometre repository root." >&2
              exit 1
            fi

            if ! command -v xcodebuild >/dev/null 2>&1; then
              echo "Xcode command line tools are required for iOS builds." >&2
              exit 1
            fi

            unset SDKROOT
            unset DEVELOPER_DIR
            export PATH="/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

            if ! xcrun --show-sdk-path --sdk iphonesimulator >/dev/null 2>&1; then
              echo "The Xcode iPhone Simulator SDK is required. Open Xcode once and install iOS simulator support." >&2
              exit 1
            fi

            export RHONOMETRE_API_BASE="''${RHONOMETRE_API_BASE:-${defaultApiBase}}"
            echo "Starting rhonometre iOS app with RHONOMETRE_API_BASE=$RHONOMETRE_API_BASE"
            cd frontend
            if ${if device then "true" else "false"}; then
              if [ -z "''${IOS_DEVICE:-}" ]; then
                echo "Set IOS_DEVICE to the connected iPhone name or UDID." >&2
                exit 1
              fi
              if [ -z "''${APPLE_TEAM_ID:-}" ]; then
                echo "Set APPLE_TEAM_ID to your Apple signing identity, for example 'Apple Development: Name (TEAMID)'." >&2
                exit 1
              fi
              exec dx serve --ios \
                --device "$IOS_DEVICE" \
                --codesign true \
                --apple-team-id "$APPLE_TEAM_ID" \
                --apple-entitlements ios/Entitlements.plist \
                --no-default-features \
                --features mobile
            else
              exec dx serve --ios --no-default-features --features mobile
            fi
          '';
        };
    in
    {
      packages = forAllSystems (system:
        {
          default = packageFor system;
          rhonometre = packageFor system;
        });

      apps = forAllSystems (system:
        {
          default = {
            type = "app";
            program = "${runnerFor system}/bin/rhonometre";
          };
          ios-simulator = {
            type = "app";
            program = "${iosRunnerFor system false}/bin/rhonometre-ios-simulator";
          };
          ios-device = {
            type = "app";
            program = "${iosRunnerFor system true}/bin/rhonometre-ios-device";
          };
          docker-build = {
            type = "app";
            program = "${dockerBuildRunnerFor system}/bin/rhonometre-docker-build";
          };
          docker-up = {
            type = "app";
            program = "${dockerUpRunnerFor system}/bin/rhonometre-docker-up";
          };
          docker-down = {
            type = "app";
            program = "${dockerDownRunnerFor system}/bin/rhonometre-docker-down";
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
              pkgs.postgresql_16
              pkgs.dioxus-cli
              pkgs.colima
              pkgs.docker
              pkgs.trunk
              pkgs.wasm-bindgen-cli
            ];

            RUST_BACKTRACE = "1";
          };
        });

      nixosModules.default = { config, lib, pkgs, ... }:
        let
          cfg = config.services.rhonometre;
        in
        {
          options.services.rhonometre = {
            enable = lib.mkEnableOption "rhonometre Axum data hub";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.default;
              description = "rhonometre package to run.";
            };
            user = lib.mkOption {
              type = lib.types.str;
              default = "rhonometre";
              description = "User that runs the service.";
            };
            group = lib.mkOption {
              type = lib.types.str;
              default = "rhonometre";
              description = "Group that runs the service.";
            };
            port = lib.mkOption {
              type = lib.types.port;
              default = 3000;
              description = "HTTP port for the Axum service.";
            };
            dataDir = lib.mkOption {
              type = lib.types.str;
              default = "/var/lib/rhonometre";
              description = "Persistent data directory for local programme files.";
            };
            environmentFile = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = "/etc/rhonometre.env";
              description = "Environment file containing DATABASE_URL, RHONOMETRE_INGEST_TOKEN, RHONOMETRE_PRO_CODE, and RHONOMETRE_TOKEN_SECRET.";
            };
          };

          config = lib.mkIf cfg.enable {
            users.groups.${cfg.group} = { };
            users.users.${cfg.user} = {
              isSystemUser = true;
              group = cfg.group;
              home = cfg.dataDir;
            };

            systemd.tmpfiles.rules = [
              "d ${cfg.dataDir} 0750 ${cfg.user} ${cfg.group} -"
              "d ${cfg.dataDir}/programmes 0750 ${cfg.user} ${cfg.group} -"
              "d /var/backups/rhonometre 0750 ${cfg.user} ${cfg.group} -"
            ];

            systemd.services.rhonometre = {
              description = "rhonometre water data hub";
              wantedBy = [ "multi-user.target" ];
              wants = [ "network-online.target" ];
              after = [ "network-online.target" "postgresql.service" ];
              environment = {
                PORT = toString cfg.port;
                STATIC_DIR = "${cfg.package}/share/rhonometre/dist";
                RHONOMETRE_PROGRAMME_DIR = "${cfg.dataDir}/programmes";
              };
              serviceConfig = {
                ExecStart = "${cfg.package}/bin/rhonometre-server";
                EnvironmentFile = lib.optional (cfg.environmentFile != null) cfg.environmentFile;
                Restart = "on-failure";
                RestartSec = "5s";
                User = cfg.user;
                Group = cfg.group;
                WorkingDirectory = cfg.dataDir;
                StateDirectory = "rhonometre";
                StateDirectoryMode = "0750";
                NoNewPrivileges = true;
                PrivateTmp = true;
              };
            };
          };
        };
    };
}
