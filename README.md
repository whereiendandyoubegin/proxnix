# Proxnix
## A function taking a git repo as an argument, returning a fully managed proxmox cluster

Proxnix is a GitOps state controller written in Rust for the Proxmox platform. It is similar in principle to ArgoCD but for Proxmox VMs rather than Kubernetes. Push to git and your VMs converge to match.

It uses Nix as the build engine so images are built atomically and errors surface before any deployment is attempted. VM configuration is defined as a nix flake output so the entire thing is a nix repo with no separate config format.

This is born of frustration with a toolchain of Ansible, Packer, Terraform and ad hoc scripts and CI jobs. The goal is to have some user defined nix expressions and VM definitions in the same repo and use those for a fully declarative, reproducible build.

State is not persisted to disk. The source of truth at all times is the nix config and live Proxmox state.

## How it works

The pipeline runs on every push:

1. Webhook received and parsed
2. Repo cloned at the pushed commit
3. All image types referenced in the config are built concurrently, as qcow2 images for VMs and tarballs for containers
4. VM config is read from the flake via `nix eval .#proxnix --json`
5. Live Proxmox state is queried via `qm` and `pct`
6. Desired state is diffed against live state
7. Workloads are created, updated in place, or destroyed as needed

Anything needing a rebuild is deployed blue/green. The new instance is provisioned into whichever slot is inactive, started, and health checked while the old one is still serving. Only once it answers is traffic cut over in sozu and the old instance retired. If any step before the cutover fails, the new instance is destroyed and the old one keeps serving.

A reconciliation loop runs every 120 seconds. Any managed VM that is stopped gets started. Any managed VM that no longer exists in Proxmox is removed from state and will be recreated on the next push.

Concurrent builds are handled by rayon. The webhook uses a semaphore to ensure only one pipeline runs at a time. Pushes arriving during a running build wait for the lock, and return 429 only if it has not freed within ten minutes.

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

This produces the binary at `result/bin/proxnix`. Run the daemon:

```bash
./result/bin/proxnix
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
      hostname = "my-server.example.com";
      blue_id = 100;
      green_id = 200;
      service_address = "192.168.1.40";
      backend_port = 80;
      image_type = "my-server";   # must match a nixosConfigurations key
      cores = 2;
      sockets = 1;
      memory_mb = 4096;
      disk_gb = 20;
      storage_location = "local-lvm";
      protected = false;
      impure = false;
    };
  };
  containers = { };
}
```

`image_type` maps a workload to the nixosConfiguration that builds its image. Multiple workloads can share the same image type.

`blue_id` and `green_id` are the two Proxmox IDs a workload alternates between. `service_address` is the stable address sozu fronts, and `backend_port` is the port the service listens on inside the instance. Omit `service_address` to leave a workload unproxied. `containers` takes the same shape as `vms` and additionally accepts `bind_mounts` for state that must survive a rebuild.

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

- Webhook authentication
- Fix remaining TODOs, there are a few places the program can panic
- TUI or web GUI for deployment status
- Flake templates to make it easier to get started without deep Nix knowledge
