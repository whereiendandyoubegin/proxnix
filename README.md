# Proxnix

Proxnix is a state controller written in rust for the proxmox platform. It uses nix as the build engine to ensure atomic images are created and error out ahead of attempted deployment.
It uses GitOps principles to deploy using a json config file, a flake, a nix qcow module and an arbitrary number of user defined nix modules.
This is born of frustration with a toolchain of Ansible, Packer, Terraform and ad hoc scripts and CI jobs. The goal is to have some user defined nix expressions and a json file with VM definitions
and use those for a fully declarative, simple build.
Another big goal is keeping state local, easily available to the daemon, and refreshed constantly.

## State of development

This is currently in an MVP state and compiles and runs on a proxmox host. A few goals for a roadmap are:
- Nix based healthchecks with sensible built in checks that can apply to any linux machine
- Fixing TODOs in the code, there are a few places where the program could panic
- Adding a TUI or web GUI for deployment
- Templating the flakes to allow for new Nix users to craft expressions for deployment a little easier

## Installation

This is a daemon which runs on the proxmox platform, as such you need to install it on the proxmox host itself.
Nix is required for installation, a Nix flake installs the software. Nixpkgs is required as a channel. 
Run `nix build`. This builds the binary in result/bin. You have to run the binary with the following args once:
- --init /path/to/your/definitions/config.json

## Usage

There are example configs in the definitions folder. The nix flake must evaluate and it must contain the qcow2 module. Otherwise it can be pretty variable.
