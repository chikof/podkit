-- Sealed MCP commands cannot be put back in plain text here; a rollback loses them.
DROP TABLE IF EXISTS auth_provider_domains;
ALTER TABLE mcp_servers DROP COLUMN command_enc;
