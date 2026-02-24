{
  description = "VM image";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }: {
    nixosConfigurations = {
      # configuration for building qcow2 images
      build-qcow2 = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          ./configuration.nix
          ./qcow.nix
        ];
      };
      build-qcow2-worker = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          ./configuration.nix
          ./qcow.nix
          ./k3s-worker.nix
        ];
      };
      build-qcow2-cp = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          ./configuration.nix
          ./qcow.nix
          ./k3s-control-plane.nix
        ];
      };
      build-qcow2-init = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          ./configuration.nix
          ./qcow.nix
          ./k3s-init.nix
        ];
      };
    };
  };
}
