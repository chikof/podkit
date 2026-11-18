# Models UI

The web pages for running models, built like the rest of the UI: server-rendered maud,
Datastar over SSE, no JavaScript build. Handlers in `server/src/ui/models/` only call the
`catalog`, `model_files`, `models` and `links` services, which authorize, scope and validate.

| Page | Path |
|---|---|
| Models and downloads of the team | `/teams/{t}/models` |
| Catalog: cards by tier, size and tag filters, fit badges for one server | `/teams/{t}/models/catalog?server=&size=&tag=` |
| Advanced: Hugging Face search | `/teams/{t}/models/catalog?tab=advanced` |
| One model's recipes, overrides, **Download** | `/teams/{t}/models/catalog/{catalog id}?server=` |
| Download: progress, log, retry/cancel/delete, then **Start** | `/teams/{t}/models/downloads/{file}` |
| A model: status, how it runs, actions, log, links, playground, usage | `/teams/{t}/projects/{p}/models/{m}` |
| Hugging Face token | `/teams/{t}/models/settings` |

The sidebar entry "Models" shows for anyone who can `read model`.

## The create flow

1. **Catalog.** Every card shows the best recipe for the selected server and its verdict from
   `catalog::suggest` (`Fits on GPU`, `GPU + RAM`, `CPU only`, `Won't fit`); the reason is the badge's
   tooltip. With no server picked (or no hardware reported yet) there are no badges.
2. **Set up.** All recipes of the model, best fit first, with download size, estimated memory and
   the explanation. Overrides: project, name, served name, context size.
3. **Download.** Queues a `model_files` download from the recipe (an identical download that is
   already queued, running or done is opened instead). The page polls every 3 s while it is in
   flight and streams the job's log. What was chosen travels in the query string, so the page can be
   reloaded.
4. **Start.** When the files are ready the page shows the start form, filled from the choices. It
   creates the model and goes to its page, which polls its status and streams the engine's log
   (the deploy's log until the engine has a container).

## Playground

A chat box on a running model's page. The server sends the conversation to the agent
(`POST /v1/gateway/chat`), which makes one streaming request to the gateway from a throwaway container
on the `podkit` network (the host cannot reach the gateway on rootless Podman) and returns the
`data:` payloads line by line. The UI appends every token to the page as an escaped `<span>`;
reasoning models' thinking shows in italics above the answer. After the answer it shows tokens,
tokens/s and the gateway's prompt/completion counts.

The chat uses a **playground key**: one per model (`playground_keys`, migration 0033), stored encrypted,
valid for that model only, put in the routing table the agent gets, never sent to the browser. Its
traffic is counted in `model_usage` like an application's and shows as "Playground" in the usage table.
The conversation lives in the browser (a signal); nothing is stored.

## Links

The model page lists the applications linked to it and the applications of its project on its server
that are not. Choosing one shows the five variables it would get, with the key masked
(`pk-••••••••••••`); linking issues the real key, which is never shown. Linking and unlinking apply to the
application's next deploy.

## Secrets

- The Hugging Face token is sent as a form post (not as a Datastar signal), stored encrypted and
  never rendered; the settings page only says Configured / Not set.
- Gateway keys (application and playground) appear nowhere in HTML or signals; the preview masks them.

## Tests and smoke

- `cargo test -p server --all-features -- ui::models`
- `scripts/ui-smoke.sh models` checks the pages. `PODKIT_SMOKE_MODEL=qwen3-4b scripts/ui-smoke.sh models`
  also downloads that model (a recipe of at most 4 GB), starts it, chats and removes everything again
  (`PODKIT_SMOKE_KEEP=1` leaves the model).
