{
  description = "Web Transport development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };

        rustTools = [
          pkgs.rustc
          pkgs.cargo
          pkgs.rustfmt
          pkgs.clippy
          pkgs.cargo-shear
          pkgs.cargo-sort
          pkgs.cargo-edit
        ];

        jsTools = [
          pkgs.nodejs_24
          pkgs.pnpm_10
          pkgs.biome
        ];

        tools = [
          pkgs.just
          pkgs.pkg-config
          pkgs.glib
          pkgs.gtk3
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          packages = rustTools ++ jsTools ++ tools;
        };
      }
    );
}