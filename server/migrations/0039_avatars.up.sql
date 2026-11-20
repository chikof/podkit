-- Profile pictures. The server decodes every upload, crops it square, resizes it to 256 px
-- and re-encodes it, so `bytes` is always a small WebP that podkit made itself.
-- `hash` is a short digest of `bytes`; the pages put it in the image URL (`?v=<hash>`) so
-- browsers can keep an avatar for a year and still see a new one at once.
-- Stored in Postgres, not on disk, so every control-plane server serves the same picture.
CREATE TABLE team_avatars (
	team_id BIGINT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
	mime TEXT NOT NULL,
	bytes BYTEA NOT NULL,
	hash TEXT NOT NULL,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE user_avatars (
	user_id BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
	mime TEXT NOT NULL,
	bytes BYTEA NOT NULL,
	hash TEXT NOT NULL,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- `teams.logo` stays: nothing reads it, and removing a NOT NULL column that other code
-- still inserts into would break an older binary during a rolling upgrade.
