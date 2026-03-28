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
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      fenix,
      crane,
      nixpkgs-unstable,
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
          }
        );

        image = pkgs.dockerTools.buildImage {
          name = "mdk-server";
          tag = "latest";
          copyToRoot = [ staticBin ];
          config = {
            Entrypoint = [ "/bin/mdk-server" ];
          };
        };
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
          integration-test = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoNextestExtraArgs = "--test integration";
            }
          );
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
              cargoNextestExtraArgs = "--bin mdk-server";
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
          ];

          env = {
            BITCOIND_EXE = "${pkgsUnstable.bitcoind}/bin/bitcoind";
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
