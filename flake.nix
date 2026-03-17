{
  description = "MDK Server";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/25.11";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixpkgs-unstable.url = "github:nixos/nixpkgs/e6f23dc08d3624daab7094b701aa3954923c6bbb";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      fenix,
      nixpkgs-unstable,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        inherit (pkgs) lib;

        pkgsUnstable = nixpkgs-unstable.legacyPackages.${system};

        rustToolchain = fenix.packages.${system}.stable.toolchain;

        nativeBuildInputs = with pkgs; [
          pkg-config
          protobuf
          rustToolchain
        ];

        buildInputs =
          with pkgs;
          [
            openssl
          ]
          ++ lib.optionals stdenv.isDarwin [
            libiconv
            darwin.apple_sdk.frameworks.Security
            darwin.apple_sdk.frameworks.SystemConfiguration
          ];
      in
      {
        devShells.default = pkgs.mkShell {
          packages =
            with pkgs;
            [
              just
              nixfmt-rfc-style
              rust-analyzer
            ]
            ++ nativeBuildInputs;

          inherit buildInputs;

          env = {
            RUSTFLAGS = "--cfg no_download";
            BITCOIND_EXE = "${pkgsUnstable.bitcoind}/bin/bitcoind";
          };

          shellHook = ''
            echo "================================================================================"
            echo "MDK Server Development Environment"



            echo "Development Environment Ready."
            echo "================================================================================"
          '';
        };
      }
    );
}
