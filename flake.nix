{
  description = "proxnix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixology.url = "git+ssh://git@forgejo.lan:2222/dan/nixology.git";
    debtmap-src = {
      url = "github:iepathos/debtmap";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, fenix, nixology, debtmap-src }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};

      toolchain = fenix.packages.${system}.stable.withComponents [
        "cargo"
        "clippy"
        "rust-src"
        "rustc"
        "rustfmt"
      ];
      rustPlatform = pkgs.makeRustPlatform {
        cargo = toolchain;
        rustc = toolchain;
      };

      rustflags = "-Clinker-features=-lld";

      commonAttrs = {
        pname = "proxnix";
        version = "0.1.0";
        src = ./nix-deployments-rs;
        cargoLock.lockFile = ./nix-deployments-rs/Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.openssl pkgs.libgit2 ];
        PROXNIX_NIXOLOGY_PATH = "${nixology}";
        RUSTFLAGS = rustflags;
      };
      proxnixPkg = rustPlatform.buildRustPackage commonAttrs;

      debtmap = rustPlatform.buildRustPackage {
        pname = "debtmap";
        version = "unstable";
        src = debtmap-src;
        cargoLock.lockFile = "${debtmap-src}/Cargo.lock";
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.openssl pkgs.libgit2 ];
        OPENSSL_NO_VENDOR = "1";
        RUSTFLAGS = rustflags;
        doCheck = false;
      };

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

      checks.${system}.clippy = rustPlatform.buildRustPackage (commonAttrs // {
        nativeBuildInputs = commonAttrs.nativeBuildInputs ++ [ toolchain ];
        buildPhase = "cargo clippy -- -W clippy::pedantic -D warnings";
        installPhase = "touch $out";
        doCheck = false;
      });

      devShells.${system}.default = pkgs.mkShell {
        inputsFrom = [ proxnixPkg ];
        packages = [
          toolchain
          fenix.packages.${system}.rust-analyzer
          pkgs.sozu
          debtmap
        ];
        RUSTFLAGS = rustflags;
      };
    };
}
