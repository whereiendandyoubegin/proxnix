{ config, lib, pkgs, ... }:

{
  imports = [ ./k3s-token.nix ];
  
  services.k3s = {
    enable = true;
    role = "agent";
    tokenFile = "/etc/rancher/k3s/token";
    serverAddr = "https://192.168.1.211:6443";
    extraFlags = [
      "--write-kubeconfig-mode=644"
    ];
  };

  networking.firewall = {
    allowedTCPPorts = [ 10250 ];
    allowedUDPPorts = [ 8472 ];
  };
}
