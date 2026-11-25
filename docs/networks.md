# Networks

Every environment gets a private Podman network on each server it has resources on
(`podkit-env-<environment id>`), so production and staging cannot reach each
other. Apps, databases and models join their environment's network the first
time they are deployed; Traefik joins only the networks that hold a routed
container, and the LLM gateway joins the networks of the apps that use a model
and of the models. Podman's shared `podkit` network is left to those two system
containers. A compose stack keeps its own network and joins others only when you
attach it.

To connect resources on purpose, open a server's **Networks** tab and create a
network (or import one that already exists on the server). Then open an
application, database, model or stack, go to its **Networks** tab and attach
the network under the names it should answer to there. Attaching is saved at
once and applies on the next deploy. A network with resources on it cannot be
deleted, and `No outbound` creates it without access to the outside.

## Moving a server that already runs resources

A resource moves to its environment's network on its next deploy. To move
everything running without a restart, use **Migrate** on the server's Networks
tab (or `POST /api/v1/teams/{team}/servers/{server}/networks/migrate`). For each
running app, database and model it connects the container to its environment's
network (and any network attached to it) and only then disconnects it from
`podkit`, so its address never disappears. Traefik does not notice a container
changing networks, so the agent starts a throwaway container after every
connect and disconnect to make it re-read the addresses (no restart). Before
that is done, a request can fail for a fraction of a second. In the live test
(`networks_live`), 64 requests sent 20 ms apart to a moved container all got
a 200.

The first run also replaces a Traefik started by an earlier version (its
config hash differs): that one is recreated, so ingress is briefly down while
the new one starts. Stacks are left alone and move on their next deploy.
Running it again is harmless.
