# Model catalog

`server/catalog/models.toml` is the curated list of models and the **recipes**
(engine x quantization x files) that run them. It is embedded into the server
with `include_str!`; `server/src/catalog/` holds the types, the pure
`fit`/`suggest` functions and the routes.

## API

| Route | Result |
|---|---|
| `GET /api/v1/catalog?server_id=&tier=&size_class=&tag=` | every model with its recipes; with `server_id`, a `best` recipe and its fit |
| `GET /api/v1/catalog/{model}?server_id=` | one model; with `server_id`, `suggestions`: the recipes ranked for that server |
| `GET /api/v1/huggingface/search?q=&limit=&server_id=` | Hugging Face GGUF repos by downloads, one variant per quantization with sizes and a fit |

`server_id` must belong to a team the caller is in (otherwise 404) and the
server must have reported its hardware (otherwise 409).

## Fit

`fit(&Hardware, &Recipe) -> Fit { kind, engine, explanation }`, in this order:

1. The download plus 10% headroom must fit the smallest reported disk, else `does_not_fit`.
2. On a usable GPU (NVIDIA, or AMD with at least 2 GB) an estimate within free VRAM is `gpu_full`.
   vLLM may only use 75% of VRAM.
3. llama.cpp and Ollama can offload: the rest in RAM is `gpu_plus_ram`.
4. Engines with a CPU mode: an estimate within RAM minus 2 GiB is `cpu_only` (`engine` is the CPU variant).
5. Otherwise `does_not_fit`.

The estimate is `models::service::estimate` at 8192 tokens, stored in the recipe
as `est_ram_mb`/`est_vram_mb` (a test keeps the two in step). A GPU verdict on a
server without an NVIDIA CDI spec says so in its explanation.

## Keeping the data honest

Repo ids, revisions and sizes are never typed by hand.

- `scripts/catalog-verify.sh` checks every recipe against the Hugging Face API
  (file lists only, no weights): the repo, files and `model_path` exist, and
  `revision` (a commit sha), `total_bytes` and the estimates match. Exit 0 means yes.
- `scripts/catalog-verify.sh --fix` pins revisions, fills bytes and estimates, and
  drops recipes (then models) that Hugging Face says do not exist. A network
  failure aborts with exit 2 and changes nothing.
- It is the only part that uses the network; `cargo test` checks the file's
  schema and invariants offline.

`verified = true` and `measured` speeds exist only for the models the engine
spike ran (`docs/spikes/engines.json`); a test compares them with that file.
