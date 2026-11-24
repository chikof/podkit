-- Uptime checks (F5): a check of a homelab service behind a self-signed certificate may skip
-- TLS verification. Off by default; the pages show a warning wherever it is on.
ALTER TABLE uptime_checks ADD COLUMN tls_skip_verify BOOLEAN NOT NULL DEFAULT FALSE;
