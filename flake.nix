{
  description = "MDK Server";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/25.11";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    nixpkgs-unstable.url = "github:nixos/nixpkgs/e6f23dc08d3624daab7094b701aa3954923c6bbb";
    vss-server = {
      url = "github:lightningdevkit/vss-server";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      fenix,
      crane,
      nixpkgs-unstable,
      vss-server,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        inherit (pkgs) lib;
        isLinux = pkgs.stdenv.isLinux;

        pkgsUnstable = nixpkgs-unstable.legacyPackages.${system};

        fenixPkgs = fenix.packages.${system};

        # Stable toolchain + musl target for static builds on Linux
        buildToolchain = fenixPkgs.combine (
          [
            (fenixPkgs.stable.withComponents [
              "cargo"
              "clippy"
              "rust-src"
              "rustc"
              "rustfmt"
            ])
          ]
          ++ lib.optionals isLinux [
            fenixPkgs.targets.${muslTarget}.stable.rust-std
          ]
        );

        craneLib = (crane.mkLib pkgs).overrideToolchain buildToolchain;

        src = craneLib.cleanCargoSource ./.;

        # Shared args for the native (non-static) build
        commonArgs = {
          inherit src;
          strictDeps = true;
          nativeBuildInputs = [ pkgs.protobuf ];
          BITCOIND_EXE = "${pkgsUnstable.bitcoind}/bin/bitcoind";
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Musl target and cross-compiler, parameterized by host architecture
        muslTarget =
          {
            x86_64-linux = "x86_64-unknown-linux-musl";
            aarch64-linux = "aarch64-unknown-linux-musl";
          }
          .${system} or null;

        muslCC =
          pkgs.pkgsCross.${
            {
              x86_64-linux = "musl64";
              aarch64-linux = "aarch64-multiplatform-musl";
            }
            .${system}
          }.stdenv.cc;

        muslTargetUnderscored = builtins.replaceStrings [ "-" ] [ "_" ] muslTarget;
        muslTargetUpperUnderscored = lib.toUpper (builtins.replaceStrings [ "-" ] [ "_" ] muslTarget);

        staticArgs = commonArgs // {
          CARGO_BUILD_TARGET = muslTarget;
          CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
          "CC_${muslTargetUnderscored}" = "${muslCC}/bin/${muslCC.targetPrefix}cc";
          "CARGO_TARGET_${muslTargetUpperUnderscored}_LINKER" = "${muslCC}/bin/${muslCC.targetPrefix}cc";
          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ muslCC ];
        };

        staticCargoArtifacts = lib.optionalAttrs isLinux (craneLib.buildDepsOnly staticArgs);

        staticBin = craneLib.buildPackage (
          staticArgs
          // {
            cargoArtifacts = staticCargoArtifacts;
            doCheck = false;
          }
        );

        image = pkgs.dockerTools.buildImage {
          name = "mdkd";
          tag = "latest";
          copyToRoot = [ staticBin ];
          config = {
            Entrypoint = [ "/bin/mdkd" ];
          };
        };

        # VSS server (lightningdevkit/vss-server) for integration tests.
        # Built with noop_authorizer so no JWT/sig config is needed.
        vssSrc = craneLib.cleanCargoSource "${vss-server}/rust";
        vssArgs = {
          src = vssSrc;
          pname = "vss-server";
          version = "0.1.0";
          strictDeps = true;
          nativeBuildInputs = [
            pkgs.protobuf
            pkgs.pkg-config
            pkgs.autoPatchelfHook
          ];
          buildInputs = [
            pkgs.openssl
            pkgs.stdenv.cc.cc.lib
          ];
          cargoExtraArgs = "--no-default-features";
          CARGO_BUILD_RUSTFLAGS = "--cfg noop_authorizer";
        };
        vssCargoArtifacts = craneLib.buildDepsOnly vssArgs;
        vss = craneLib.buildPackage (
          vssArgs
          // {
            cargoArtifacts = vssCargoArtifacts;
            doCheck = false;
          }
        );
      in
      {
        packages = {
          default = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              doCheck = false; # Tests run in checks.test
            }
          );
          # Wrapper script that runs `just integration-test` with all
          # dependencies available.  Intended for CI (`nix run .#integration-test`).
          integration-test = pkgs.writeShellApplication {
            name = "integration-test";
            runtimeInputs = [
              buildToolchain
              pkgs.cargo-nextest
              pkgs.just
              pkgs.postgresql_16
              pkgs.curl
              pkgs.protobuf
              pkgsUnstable.bitcoind
              vss
            ];
            text = ''
              export BITCOIND_EXE="${pkgsUnstable.bitcoind}/bin/bitcoind"
              export VSS_EXE="${vss}/bin/vss-server"
              export NIX_SYSTEM="${system}"
              just integration-test "$@"
            '';
          };
          inherit vss;
        }
        // lib.optionalAttrs isLinux {
          static = staticBin;
          image = image;
        };

        checks = {
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "-- --deny warnings";
            }
          );

          fmt = craneLib.cargoFmt { inherit src; };

          test = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoNextestExtraArgs = "--bin mdkd";
            }
          );
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};

          packages = with pkgs; [
            cargo-nextest
            just
            nixfmt-rfc-style
            grpcurl
            jq
            unixtools.xxd
            microsocks
            postgresql_16
          ];

          env = {
            BITCOIND_EXE = "${pkgsUnstable.bitcoind}/bin/bitcoind";
            VSS_EXE = "${vss}/bin/vss-server";
            NIX_SYSTEM = system;
          };

          shellHook = ''
            echo "================================================================================"
            echo "MDK Server Development Environment"

            echo "Configuring Project..."
            git config core.hooksPath .githooks

            echo "Development Environment Ready."
            echo "================================================================================"
          '';
        };
      }
    );
}
