# Spike: GPU passthrough for rootless Podman via CDI

**Result: it works through Podman's Docker-compatible API, with the bollard
`HostConfig` below.** Verified on rootless Podman 6.1.4, RTX 5070, driver
595.112.07, image `docker.io/nvidia/cuda:12.8.1-base-ubuntu24.04`.

## Working variant (use this one)

```rust
HostConfig {
    devices: Some(vec![DeviceMapping {
        path_on_host: Some("nvidia.com/gpu=all".to_string()), // CDI name, not a path
        ..Default::default()
    }]),
    ..Default::default()
}
```

This is the API equivalent of `podman run --device nvidia.com/gpu=all`.
Podman resolves a device that looks like a CDI name through the CDI registry.
Use `nvidia.com/gpu=0` (or a UUID) for one GPU. No `path_in_container` or
`cgroup_permissions` are needed.

`device_requests: [{driver: "cdi", device_ids: ["nvidia.com/gpu=all"]}]` also
works, with the same result. It is the shape Docker 28+ uses, so it is the one
to switch to if the agent ever targets Docker. Podkit only targets Podman, so
use `devices`.

## Variants that failed

Both create the container, then fail at start (HTTP 500):

```
crun: executable file `nvidia-smi` not found in $PATH: No such file or directory: OCI runtime attempted to invoke a command that was not found
```

- `device_requests: [{device_ids: ["nvidia.com/gpu=all"]}]` (no driver)
- `device_requests: [{count: -1, capabilities: [["gpu"]]}]` (Docker `--gpus all` style)

Podman ignores them without an error, so nothing is injected. The failure only
shows up as a missing binary or library, so an agent must not treat "create
succeeded" as proof the GPU is attached.

The libpod API fallback was not needed.

## Proof

- CLI: `podman run --rm --device nvidia.com/gpu=all docker.io/nvidia/cuda:12.8.1-base-ubuntu24.04 nvidia-smi -L`
  prints `GPU 0: NVIDIA GeForce RTX 5070 (UUID: ...)`.
- Test: `devenv shell -- cargo test -p agent --test gpu_cdi -- --ignored --nocapture`
  runs all four variants through the agent's bollard client and prints `[OK]`
  or `[FAIL]` for each. It asserts at least one works.

## Rootless caveats

- Needs a CDI spec. NixOS generates it with
  `hardware.nvidia-container-toolkit.enable = true;`. The spec is at
  `/run/cdi/nvidia-container-toolkit.json` (`/var/run` is a symlink to `/run`).
  There is no `/etc/cdi`. It declares `nvidia.com/gpu=all`, `=0`, and by UUID.
- No root and no `--privileged` are needed, because the `/dev/nvidia*` nodes
  are world-rw (`crw-rw-rw-`) on this host. A host with stricter modes would
  need group access for the rootless user (not tested here).
- The CUDA *base* image has no CUDA libs, only what the CDI spec injects.
  That is enough for `nvidia-smi`. Inference images carry their own runtime.

## Detecting "this server can do GPUs" without root

Cheap checks the agent can run (they feed card AI 01's hardware inventory):

1. Any `*.json` or `*.yaml` in `/etc/cdi`, `/var/run/cdi`, `/run/cdi` that
   declares a `kind` such as `nvidia.com/gpu`. The device names are
   `<kind>=<name>` from `devices[].name`. This is the authoritative check,
   because it is exactly what Podman will accept.
2. `nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader`
   on the host, for the model name and VRAM. A GPU with no CDI spec means "has a
   GPU, toolkit not set up", which the UI can show as a fix-it hint.
None of this needs root.

## AMD / Vulkan

Not tested. The host has `/dev/kfd` (rw for all) and `/dev/dri/renderD128/129`
(`root:render`, rw for all), so ROCm or Vulkan containers could use plain
`devices: [{path_on_host: "/dev/kfd"}, {path_on_host: "/dev/dri"}]`
without CDI. AMD can also use CDI (`amd.com/gpu=all`), but this host has no
spec for it. Out of scope for this spike.
