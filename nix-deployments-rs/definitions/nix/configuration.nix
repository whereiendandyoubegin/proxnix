{ config, lib, pkgs, ... }: {

  networking = {
    hostName = "k3s-nix-node";
    useDHCP = lib.mkDefault true;
    firewall.enable = true;
  };

  # SSH user
  users = {
    mutableUsers = false;
    users.root.hashedPassword = "!";
    users.dan = {
      isNormalUser = true;
      openssh.authorizedKeys.keys = [
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAyMF4IlwH6a3oc6m5vjZxVUAaA8wGoy+dF8TXKEvU/p gilmour890@gmail.com"
      ];
    };
  };

  # SSH on port 22
  services.openssh = {
    enable = true;
    ports = [ 22 ];
    settings = {
      PasswordAuthentication = false;
      PermitRootLogin = "no";
      };
  };


  # Virtualisation utils
  boot.initrd.availableKernelModules = [
    "virtio_pci" "virtio_blk" "virtio_scsi" "virtio_net"
    "ahci" "sd_mod" "xhci_pci" "nvme"
  ];

  services.qemuGuest.enable = true;
  services.cloud-init.enable = true;

  # System packages
  # environment.systemPackages = with pkgs; [
  #   git helix screen tmux wget curl sudo nmap btop iftop
  #   nix qemu-utils nfs-utils iotop k3s k9s kubectl
  #   kubectx kustomize kubernetes-helm helmfile argocd fluxcd
  #   tree podman prometheus grafana-loki promtail grafana
  #   openssl openssh rclone go ripgrep 
  # ];
  #
  environment.systemPackages = with pkgs; [
  qemu-utils
  git
  curl wget
  vim helix tmux screen
  btop iftop iotop
  nfs-utils
  k3s
];


  # Security
  security.sudo.enable = true;
  security.sudo.wheelNeedsPassword = false;
  nix.settings.allowed-users = [ ];
  users.users.dan.extraGroups = [ "wheel" ];
  security.sudo.extraRules = [{
  users = [ "dan" ];
  commands = [{
    command = "ALL";
    options = [ "NOPASSWD" ];
  }];
}];

  users.users.root.openssh.authorizedKeys.keys = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAyMF4IlwH6a3oc6m5vjZxVUAaA8wGoy+dF8TXKEvU/p gilmour890@gmail.com"
  ];
  
  system.stateVersion = "24.11";
}
