-- Files a template mounts into its stack (config files with the template's
-- variables filled in). age-encrypted JSON object, path -> content; empty
-- for stacks without any.
ALTER TABLE stacks ADD COLUMN files BYTEA NOT NULL DEFAULT '';
