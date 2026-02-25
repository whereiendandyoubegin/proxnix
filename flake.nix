{
  description = "proxnix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      packages.${system}.default = pkgs.rustPlatform.buildRustPackage {
        pname = "proxnix";
        version = "0.1.0";
        src = ./nix-deployments-rs;
        cargoLock.lockFile = ./nix-deployments-rs/Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.openssl pkgs.libgit2 ];
      };

      devShells.${system}.default = pkgs.mkShell {
        buildInputs = [ pkgs.openssl pkgs.libgit2 pkgs.pkg-config pkgs.rustup ];
      };
    };
}
