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
          pkgs.cargo-hack
          pkgs.just
          pkgs.bun
          pkgs.pkg-config
          pkgs.glib
          pkgs.gtk3
          # Required to compile boringssl (via bindgen loading libclang)
          pkgs.llvmPackages.libclang.lib
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          packages = tools;

          shellHook = ''
            export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath [pkgs.llvmPackages.libclang.lib]}:$LD_LIBRARY_PATH
          '';
        };
      }
    );
}
