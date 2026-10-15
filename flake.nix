{
  description = "dbd — database schema management as code";

  inputs = {
    nixpkgs = {
      url = "github:NixOS/nixpkgs/nixos-unstable";
    };
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      mkDbd = pkgs: pkgs.callPackage ./nix/derivations/dbd.nix { };
    in
    {
      packages = forAllSystems (pkgs: rec {
        dbd = mkDbd pkgs;
        default = dbd;
      });

      apps = forAllSystems (pkgs: rec {
        dbd = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.dbd}/bin/dbd";
        };
        default = dbd;
      });

      overlays.default = final: _prev: {
        dbd = mkDbd final;
      };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ (mkDbd pkgs) ];
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
          ];
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
