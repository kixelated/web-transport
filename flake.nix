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

        tools = [
          pkgs.rustc
          pkgs.cargo
          pkgs.rustfmt
          pkgs.clippy
          pkgs.cargo-shear
          pkgs.cargo-sort
          pkgs.cargo-edit
          pkgs.just
          pkgs.pkg-config
          pkgs.glib
          pkgs.gtk3
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          packages = tools;
        };
      }
    );
}
