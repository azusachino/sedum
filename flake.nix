{
  description = "sedum — filesystem-owned personal Markdown wiki";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            # rust toolchain
            rustc
            cargo
            rustfmt
            clippy
            # python scripting/automation (run via `uv run`, replaces bash glue)
            uv
            # formatting
            prettier
          ];
        };
      });
}
