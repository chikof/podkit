-- What the agent's health check last said about Podman: its version and whether it runs
-- rootless. The status sync writes both every sweep; the sidebar's agent card reads them.
-- Null until the first answer, and `podman_rootless` stays null for an older agent.
ALTER TABLE servers ADD COLUMN podman_version VARCHAR(40);
ALTER TABLE servers ADD COLUMN podman_rootless BOOLEAN;
