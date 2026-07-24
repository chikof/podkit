ALTER TABLE teams DROP CONSTRAINT teams_slug_key;
ALTER TABLE teams DROP COLUMN updated_at;
ALTER TABLE teams ADD COLUMN owner_id BIGINT REFERENCES "users"(id);
