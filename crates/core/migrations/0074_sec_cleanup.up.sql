-- Two things for the security clean-up:
--
-- 1. An MCP stdio server's command line often carries a token, so it is stored encrypted like any
--    other secret. `command` (plain text) stays for rows written before this: podkit seals them
--    at start-up and clears the column.
ALTER TABLE mcp_servers ADD COLUMN command_enc BYTEA;

-- 2. The email domains a team's identity provider may create accounts for (SEC-07). A domain
--    counts once an instance administrator approved it; `approved_at IS NULL` waits.
CREATE TABLE auth_provider_domains (
	provider_id BIGINT NOT NULL REFERENCES auth_providers(id) ON DELETE CASCADE,
	domain TEXT NOT NULL,
	approved_at TIMESTAMPTZ,
	approved_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
	PRIMARY KEY (provider_id, domain)
);

-- A team administrator's own provider cannot vouch for an unverified email.
UPDATE auth_providers SET allow_unverified_email = FALSE WHERE team_id IS NOT NULL;
