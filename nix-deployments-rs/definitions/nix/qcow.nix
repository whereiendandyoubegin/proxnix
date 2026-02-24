{ config, lib, pkgs, modulesPath, ... }: {
  imports = [
    "${toString modulesPath}/profiles/qemu-guest.nix"
  ];

  fileSystems."/" = {
    device = "/dev/disk/by-label/nixos";
    autoResize = true;
    fsType = "ext4";
  };

  boot.kernelParams = [ "console=tty0" "console=ttyS0,115200n8" ];
  boot.loader.grub.device = lib.mkDefault "/dev/vda";
  boot.loader.grub.enable = true;

  system.build.qcow2 = import "${modulesPath}/../lib/make-disk-image.nix" {
    inherit lib config pkgs;
    diskSize = 4000;
    format = "qcow2";
    partitionTableType = "hybrid";
  };
}
