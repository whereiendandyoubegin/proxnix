{ config, lib, pkgs, ... }:

{
  imports = [ ./k3s-token.nix ];
  
  services.k3s = {
    enable = true;
    role = "server";
    tokenFile = "/etc/rancher/k3s/token";
    clusterInit = lib.mkDefault false;
    serverAddr = lib.mkDefault "https://192.168.1.211:6443";
    extraFlags = [
      "--cluster-cidr=10.42.0.0/16"
      "--service-cidr=10.43.0.0/16"
      "--tls-san=${config.networking.hostName}"
      "--write-kubeconfig-mode=644"
    ];
  };

  networking.firewall = {
    allowedTCPPorts = [ 6443 10250 2379 2380 ];
    allowedUDPPorts = [ 8472 ];
  };
}
