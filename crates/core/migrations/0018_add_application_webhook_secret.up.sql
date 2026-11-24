-- age-encrypted. NULL means webhook-triggered deploys are disabled for this
-- app (e.g. it predates this column); the client re-creates the app or a
-- future "rotate webhook secret" endpoint backfills it.
ALTER TABLE applications ADD COLUMN IF NOT EXISTS webhook_secret BYTEA;
