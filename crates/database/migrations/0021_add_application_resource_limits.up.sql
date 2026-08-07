-- NULL = unlimited (podman/docker default)
ALTER TABLE applications ADD COLUMN IF NOT EXISTS memory_limit_mb INTEGER;
ALTER TABLE applications ADD COLUMN IF NOT EXISTS cpu_limit REAL;
