-- Instance-wide settings (SEC-02/03): a key and a value, for what belongs to the whole podkit
-- instance and not to a team. The first is `outbound_private`: `allow` or `deny`, whether the
-- control plane may make requests to private network addresses (see `crates/outbound`). An
-- absent row means "the default", which follows the registration policy.
CREATE TABLE IF NOT EXISTS instance_settings (
	key        TEXT PRIMARY KEY,
	value      TEXT NOT NULL,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
