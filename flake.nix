{
  description = "proxnix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nixology.url = "git+ssh://git@ssh.dan-gilmour.com:2222/dan/nixology.git";
  };

  outputs = { self, nixpkgs, nixology }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      commonAttrs = {
        pname = "proxnix";
        version = "0.1.0";
        src = ./nix-deployments-rs;
        cargoLock.lockFile = ./nix-deployments-rs/Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.openssl pkgs.libgit2 ];
        PROXNIX_NIXOLOGY_PATH = "${nixology}";
      };
      proxnixPkg = pkgs.rustPlatform.buildRustPackage commonAttrs;

      sozuConfig = pkgs.writeText "sozu-config.toml" ''
        command_socket = "/run/sozu/command.sock"
        log_level      = "info"
        log_target     = "stdout"
        command_buffer_size     = 16384
        max_command_buffer_size = 163840

        [[listeners]]
        protocol = "http"
        address  = "0.0.0.0:80"
      '';

      sozuUnit = pkgs.writeText "sozu.service" ''
        [Unit]
        Description=Sozu reverse proxy
        After=network.target

        [Service]
        Type=simple
        ExecStart=${pkgs.sozu}/bin/sozu start --config /etc/sozu/config.toml
        Restart=always
        RestartSec=5
        RuntimeDirectory=sozu
        RuntimeDirectoryMode=0755

        [Install]
        WantedBy=multi-user.target
      '';

      proxnixUnit = pkgs.writeText "proxnix.service" ''
        [Unit]
        Description=Proxnix deployment pipeline
        After=network.target sozu.service
        Requires=sozu.service

        [Service]
        Type=simple
        Environment=PATH=/nix/var/nix/profiles/default/bin:/usr/local/bin:/usr/bin:/bin
        ExecStart=${proxnixPkg}/bin/proxnix
        Restart=always
        RestartSec=5

        [Install]
        WantedBy=multi-user.target
      '';

      installScript = pkgs.writeShellApplication {
        name = "proxnix-install";
        text = ''
          echo "Installing proxnix and sozu..."

          mkdir -p /etc/sozu
          cp ${sozuConfig}  /etc/sozu/config.toml
          cp ${sozuUnit}    /etc/systemd/system/sozu.service
          cp ${proxnixUnit}  /etc/systemd/system/proxnix.service

          systemctl daemon-reload
          systemctl enable --now sozu
          systemctl enable --now proxnix

          echo "Done. sozu and proxnix are running."
        '';
      };
    in {
      packages.${system} = {
        default = proxnixPkg;
        sozu = pkgs.sozu;
      };

      apps.${system}.install = {
        type = "app";
        program = "${installScript}/bin/proxnix-install";
      };

      checks.${system}.clippy = pkgs.rustPlatform.buildRustPackage (commonAttrs // {
        nativeBuildInputs = commonAttrs.nativeBuildInputs ++ [ pkgs.clippy ];
        buildPhase = "cargo clippy -- -D warnings";
        installPhase = "touch $out";
        doCheck = false;
      });

      devShells.${system}.default = pkgs.mkShell {
        buildInputs = [ pkgs.openssl pkgs.libgit2 pkgs.pkg-config pkgs.rustup pkgs.sozu ];
      };
    };
}
