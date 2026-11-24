-- Sign-in throttling (SEC-01): one row per wrong (or still undecided) guess at a password,
-- a second-factor code, a passkey, an SSO link password or an API token.
--
-- `subject` says who is being guarded: `u:<user id>` for a known account, `e:<sha256 of the
-- email>` for an email nobody has (so an unknown email locks exactly like a known one),
-- `ip:<address>` for sign-in guesses from one address across accounts and `tip:<address>`
-- for wrong API tokens from one address. The service counts the rows of the last 15 minutes
-- and asks for a growing wait once there are too many; a full sign-in deletes the account's
-- rows. The counters live here, not in memory, so a restart or a second replica sees them.
CREATE TABLE auth_failures (
	id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
	subject TEXT NOT NULL,
	at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX auth_failures_subject_at ON auth_failures (subject, at DESC);
CREATE INDEX auth_failures_at ON auth_failures (at);
