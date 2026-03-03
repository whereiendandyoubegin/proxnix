# Proxnix

Proxnix is a GitOps state controller written in Rust for the Proxmox platform. It is similar in principle to ArgoCD but for Proxmox VMs rather than Kubernetes. Push to git and your VMs converge to match.

It uses Nix as the build engine so images are built atomically and errors surface before any deployment is attempted. VM configuration is defined as a nix flake output so the entire thing is a nix repo with no separate config format.

This is born of frustration with a toolchain of Ansible, Packer, Terraform and ad hoc scripts and CI jobs. The goal is to have some user defined nix expressions and VM definitions in the same repo and use those for a fully declarative, reproducible build.

State is not persisted to disk. The source of truth at all times is the nix config and live Proxmox state.

## How it works

The pipeline runs on every push:

1. Webhook received and parsed
2. Repo cloned at the pushed commit
3. All `nixosConfigurations` in the flake are built as qcow2 images concurrently
4. VM config is read from the flake via `nix eval .#proxnix --json`
5. Live Proxmox state is queried via `qm`
6. Desired state is diffed against live state
7. VMs are created, updated in place, or destroyed as needed

A reconciliation loop runs every 10 seconds. Any managed VM that is stopped gets started. Any managed VM that no longer exists in Proxmox is removed from state and will be recreated on the next push.

Concurrent builds are handled by rayon. The webhook uses a semaphore to ensure only one pipeline runs at a time. Duplicate pushes during a running build return 429.

## Requirements

- Proxmox host
- Nix installed on the Proxmox host
- SSH key at `/root/.ssh/id_ed25519`, `/root/.ssh/id_rsa`, or `/root/.ssh/id_ecdsa` with read access to your repo
- Git server capable of sending push webhooks

## Installation

Clone this repo onto your Proxmox host and run:

```bash
nix build
```

This produces the binary at `result/bin/nix-deployments-rs`. Run it once with `--init` to create required directories:

```bash
./result/bin/nix-deployments-rs --init
```

Then run the daemon:

```bash
./result/bin/nix-deployments-rs
```

It listens on `0.0.0.0:6780`. Point your git server's push webhook at `http://<host>:6780/whlisten`.

## Repo structure

Your nix repo needs two things.

**`nixosConfigurations` in your flake**, one per VM image type, each using the qcow2 module:

```nix
nixosConfigurations = {
  my-server = nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [ ./configuration.nix ./qcow.nix ./my-server.nix ];
  };
};
```

**A `proxnix` flake output** defining which VMs to deploy. The cleanest way is to keep this in a separate file and import it:

```nix
proxnix = import ./proxnix.nix;
```

```nix
# proxnix.nix
{
  vms = {
    "my-server" = {
      name = "my-server";
      vm_id = 100;
      image_type = "my-server";   # must match a nixosConfigurations key
      cores = 2;
      sockets = 1;
      memory_mb = 4096;
      disk_gb = 20;
      storage_location = "local-lvm";
      cloud_init = "None";
      protected = false;
    };
  };
}
```

`image_type` maps a VM to the nixosConfiguration that builds its disk image. Multiple VMs can share the same image type.

Verify the config evaluates correctly before pushing:

```bash
nix eval .#proxnix --json | jq .
```

There is an example repo at https://github.com/whereiendandyoubegin/proxnix-example.

## State of development

This runs in production on a Proxmox homelab and is in active development. Known limitations:

- A few unwrap calls that can panic on malformed qm output
- No authentication on the webhook endpoint
- Single node Proxmox only

## Roadmap

- Nix based healthchecks with sensible built in defaults for any linux machine
- Webhook authentication
- Fix remaining TODOs, there are a few places the program can panic
- TUI or web GUI for deployment status
- Flake templates to make it easier to get started without deep Nix knowledge
