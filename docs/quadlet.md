# Quadlet view and eject

Quadlet is how Podman runs containers under systemd: a `.container` file (plus `.network`
and `.volume` files) in `~/.config/containers/systemd/` (rootless) or
`/etc/containers/systemd/` (rootful) becomes a systemd service after
`systemctl --user daemon-reload`.

podkit can show what it runs as Quadlet units, and hand you all of them to leave podkit
("eject"). Nothing here changes how podkit itself runs containers: podkit-agent still talks
to Podman's API, and podkit never reads these units back.

## The Quadlet tab

Applications, databases, models and stacks have a **Quadlet** tab. It shows each generated
unit read-only, with the path it would go to and a Copy button, and how to start them. A
stack shows one container unit per compose service, ordered by `depends_on`.

The tab needs `read` on the resource and `read env_var`. The same thing is in the API:
`GET …/applications/{id}/quadlet` (and `databases`, `models`, `stacks`), see
[api.md](api.md#quadlet).

## Eject

**Eject to Quadlet** in a project's menu (or **Team settings → Eject to Quadlet** for every
project) downloads a `.tar.gz`:

```
podkit-quadlet-shop/
  node-01/
    README.md
    units/
      podkit-env-3.network
      podkit-db-7-data.volume
      podkit-db-7.container
      podkit-app-12.container
      podkit-app-12.env          (only with secrets)
      podkit-files/stack-4-web/1-nginx.conf   (only with secrets)
```

One folder per server: copy each to its server. The README says how to install the units,
the order to start them in, and what podkit stops doing. In short:

```sh
mkdir -p ~/.config/containers/systemd
cp -r units/. ~/.config/containers/systemd/
chmod 600 ~/.config/containers/systemd/*.env
systemctl --user daemon-reload
loginctl enable-linger "$USER"
systemctl --user start podkit-db-7.service podkit-app-12.service
```

Units cannot be `systemctl enable`d: their `[Install] WantedBy=default.target` starts them at
boot once linger is on.

Stop podkit's own containers of those resources first (or delete the resources in podkit):
otherwise both copies fight over ports and volumes. Volumes keep their podkit names, so a unit
started on the same server finds the data.

### Secrets

Units never contain a secret. A container's environment goes into `podkit-<service>.env`,
which the unit reads with `EnvironmentFile=`; the unit lists the variable names in a comment.
A stack's mounted files (configs) go under `podkit-files/` the same way.

These files are only in an export that asks for them (the **Include the env files** box, or
`secrets=true`). That needs what revealing a secret needs: a session or an API token with
`read:sensitive`, and `update` on env vars, databases, models and stacks. Every export is
written to the audit log as `quadlet.export`, saying whether secrets went out. The files are
written mode 0600 in the archive.

Shared variables and references are resolved when you export: the env file holds the values
of that moment. A value of several lines cannot be in an env file; it is left out with a
comment naming it.

## What a unit contains

The units come from the same deploy spec the agent runs, so they match it:

| podkit setting | Quadlet |
|---|---|
| image | `Image=`; a pulled tag that is fully qualified also gets `AutoUpdate=registry` (`podman auto-update`); an image podkit built or pinned to a digest gets `Pull=never` |
| git application | the image podkit built (`Pull=never`); never deployed: `localhost/podkit-<service>:latest`, which you build, and a comment with the repository (credentials stripped) |
| container name | `ContainerName=podkit-<service>`, without the deployment id podkit adds |
| command, entrypoint | `Exec=`, `Entrypoint=` |
| environment | `EnvironmentFile=` (see Secrets) |
| networks and aliases | `Network=<name>.network:alias=…`, and a `.network` unit with `NetworkName=` (and `Internal=true`) |
| volumes | `Volume=<name>.volume:<path>[:ro]`, and a `.volume` unit with `VolumeName=` |
| published ports | `PublishPort=[ip:]host:container[/udp]` |
| health check (command) | `HealthCmd=`, `HealthInterval=`, `HealthTimeout=`, `HealthRetries=`, `HealthStartPeriod=` |
| memory, CPU, `/dev/shm`, extra groups | `PodmanArgs=--memory=…`, `--cpus=…`, `--shm-size=…`, `--group-add=…` |
| GPUs | `AddDevice=nvidia.com/gpu=…` (CDI) or `/dev/kfd` and `/dev/dri` |
| user, workdir, hostname, read-only, init, tmpfs, hosts, DNS, capabilities, sysctls, ulimits, stop timeout, labels | `User=`/`Group=`, `WorkingDir=`, `HostName=`, `ReadOnly=`, `RunInit=`, `Tmpfs=`, `AddHost=`, `DNS=`, `AddCapability=`/`DropCapability=`, `Sysctl=`, `Ulimit=`, `StopTimeout=`, `Label=` |
| restart | `[Service] Restart=always` (or `no`) |
| replicas | one unit per replica (`podkit-app-12-r2` …); only the first publishes ports |
| compose `depends_on` | `Requires=` and `After=` on the other service's unit |

Left out, each with a comment in the unit saying so:

- **Traefik routing**: domains, HTTPS and access rules are Traefik labels and middlewares.
  The unit names the hosts it served; publish a port or run your own proxy.
- **The HTTP health gate**: podkit's HTTP check runs podkit-agent inside the container. The
  unit suggests a `HealthCmd=` to adapt, commented out.
- **The MCP bridge** of a stdio MCP server, which is podkit-agent too.
- **The `podkit.service` label**, so podkit-agent on the same host does not take the
  container for its own.
- **Logging**: podkit uses a 10 MB `k8s-file` log; under systemd the journal has the logs.

## What stops working

podkit does none of this for containers started from the units:

- Traefik routing, generated and custom domains, certificates and access rules.
- Deploys: git pushes, webhooks, previews, rollbacks, and builds of git applications.
- Health-gated and rolling deploys, and load balancing over replicas.
- Backups and restores of databases and volumes.
- Cron jobs, uptime checks, notifications, metrics, monitoring, log search, the terminal and
  image scanning.
- The model gateway, the MCP gateway and its bridge, Tailscale sidecars.
- Variable changes made in podkit afterwards.
- podkit-agent's supervisor: systemd restarts the containers instead.

## Where the code is

- `crates/proto/src/quadlet.rs`: the generator, a pure function of a `DeploySpec`, and the
  README. Unit-tested (`cargo test -p proto quadlet`). It lives in `proto` so the agent can
  use it too.
- `crates/core/src/quadlet/`: builds each resource's spec through its deploy path
  (`deployments::run::current_spec`, `databases::run::full_spec`, `models::run::full_spec`,
  `stacks::run::plan`), the API, and the archive.
- `crates/ui/src/pages/quadlet.rs`: the tab body and the export page and card.
