-- Email invites to a team (`members::invites`). A member with `create membership` invites an
-- address with a role; the link that goes out carries a random token, and only its SHA-256 is
-- kept here, like a session's. An invite works once: accepting it (or revoking it) deletes the
-- row, and the audit log keeps the history. It stops working at `expires_at`. One open invite
-- per address and team; a new one for the same address replaces an expired one.
CREATE TABLE team_invites (
	id BIGINT PRIMARY KEY NOT NULL,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	-- Lower case, so one address has one invite whatever case it was typed in; the same length
	-- limit as `users.email`.
	email VARCHAR(100) NOT NULL CHECK (email = lower(email)),
	role_id BIGINT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
	token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
	-- An invite is only as good as its sender: it goes with their account (and is refused or
	-- pruned once they may no longer make it, see `members::invites`).
	invited_by BIGINT REFERENCES users(id) ON DELETE CASCADE,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	expires_at TIMESTAMPTZ NOT NULL
);

CREATE UNIQUE INDEX team_invites_team_email ON team_invites (team_id, email);

-- One row per invite email sent (a new invite or a resend), for the per-team hourly limit.
-- Kept apart from `team_invites` so revoking and re-inviting does not reset the count.
CREATE TABLE team_invite_sends (
	id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	team_id BIGINT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
	at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX team_invite_sends_team_at ON team_invite_sends (team_id, at DESC);
