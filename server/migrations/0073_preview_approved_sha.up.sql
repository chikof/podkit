-- A fork pull request's preview is built from the commit a maintainer approved with `/preview`,
-- not from whatever the branch holds when the clone happens. Empty: nothing is approved.
ALTER TABLE previews ADD COLUMN approved_sha VARCHAR(64) NOT NULL DEFAULT '';
UPDATE previews SET approved_sha = head_sha WHERE approved;
