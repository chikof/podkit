# colibri engine image

podkit's `colibri` engine runs `localhost/podkit-colibri:cpu` (or `:cuda` when the model
uses a GPU). [colibri](https://github.com/JustVugg/colibri) ships no official image, so
for now **building it is a manual step on each server that will run colibri models**.
Publishing it to a registry is not done yet; when it is, change the two image names in
`server/src/models/engines.rs` and set `pull: true` there.

The image is built from `Containerfile` here, which pins colibri to the commit the engine
spike measured (`0a5504a1dc64f86fcd0fe7db298b0448c730e7e6`, banner `v2.1.0`) and builds the
`olmoe` engine only. The recipe and measurements are in `docs/spikes/engines.md`.

## Build

Run these as the user whose Podman podkit-agent talks to (the same one that runs the
agent), from the repository root. Podman keeps images per user, so building as another
user does nothing for podkit.

CPU, about a minute, 126 MB:

```sh
podman build -t localhost/podkit-colibri:cpu \
  -f deploy/engines/colibri/Containerfile deploy/engines/colibri
```

CUDA, about five minutes, 3.5 GB. It builds for every GPU architecture (`CUDA_ARCH=portable`),
which covers Blackwell (sm_120):

```sh
podman build -t localhost/podkit-colibri:cuda \
  --build-arg BUILD_BASE=docker.io/nvidia/cuda:12.8.1-devel-ubuntu24.04 \
  --build-arg RUN_BASE=docker.io/nvidia/cuda:12.8.1-runtime-ubuntu24.04 \
  --build-arg MAKE_ARGS="CUDA=1 CUDA_ARCH=portable" \
  -f deploy/engines/colibri/Containerfile deploy/engines/colibri
```

The CUDA build pulls about 5 GB of NVIDIA base images the first time.

Check it:

```sh
podman image exists localhost/podkit-colibri:cpu && echo cpu ok
podman image exists localhost/podkit-colibri:cuda && echo cuda ok
```

If the image is missing when a colibri model starts, the start fails with
`image localhost/podkit-colibri:cpu no longer exists on this server`. Build it and start
the model again.

## How podkit runs it

- The model files volume is mounted read-only at `/models`. `entrypoint.sh` imports a GGUF
  in link mode (nothing is copied) and runs `coli serve`; a prepared `*.colibri` directory
  is used as it is.
- Environment set by podkit: `COLI_GGUF=/models/<file>` or `COLI_MODEL_DIR=/models/<dir>`,
  `COLI_PORT=8000`, `COLI_ALLOW_INSECURE_BIND=1` (colibri refuses to bind `0.0.0.0` without
  an API key; the container is only on the internal `podkit` network), and `COLI_CUDA=1`
  for GPU models. Without `COLI_CUDA=1` even the CUDA image runs on the CPU.
- Arguments: `--ctx <context_size> --model-id <served_name>`, then the model's extra arguments.
- Readiness is `GET /health`.
- Only OLMoE is built into the image. Other colibri model families need another `ENGINE`
  build argument (see the comments at the top of the `Containerfile`).
