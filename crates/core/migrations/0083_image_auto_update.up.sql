-- Image auto-update: an application deployed from a registry image can ask podkit to check
-- its tag on a schedule and redeploy, through the normal deploy, when the tag names a new
-- digest. Off by default; an application without a row is never checked.
CREATE TABLE image_auto_update (
	application_id BIGINT PRIMARY KEY REFERENCES applications(id) ON DELETE CASCADE,
	enabled BOOLEAN NOT NULL DEFAULT FALSE,
	-- How often the tag is checked.
	check_interval VARCHAR(6) NOT NULL DEFAULT 'daily'
		CHECK (check_interval IN ('hourly', '6h', 'daily')),
	-- An optional daily maintenance window, minutes of the day in a fixed UTC offset; the end
	-- may be before the start, which wraps over midnight. Scheduled checks happen only inside
	-- it. Both set or both empty.
	window_start_minute INTEGER CHECK (window_start_minute BETWEEN 0 AND 1439),
	window_end_minute INTEGER CHECK (window_end_minute BETWEEN 0 AND 1439),
	utc_offset_minutes INTEGER NOT NULL DEFAULT 0 CHECK (utc_offset_minutes BETWEEN -840 AND 840),
	-- When the scheduler looks next; NULL while off.
	next_check_at TIMESTAMPTZ,
	last_checked_at TIMESTAMPTZ,
	-- What the last check came to:
	--   up_to_date      the tag names the digest that runs
	--   deployed        the tag moved and a deployment was queued
	--   skipped_bad     the tag names a digest whose auto-update failed or was rolled back
	--   busy            a deployment was already in progress
	--   frozen          an autodeploy freeze window is open
	--   outside_window  outside the maintenance window
	--   not_running     the application has no running deployment to update
	--   error           the registry or the agent could not be asked
	last_result VARCHAR(14) CHECK (last_result IN (
		'up_to_date', 'deployed', 'skipped_bad', 'busy', 'frozen', 'outside_window',
		'not_running', 'error'
	)),
	last_detail VARCHAR(500) NOT NULL DEFAULT '',
	-- The digest the tag named at the last check that reached the registry.
	remote_digest VARCHAR(140),
	-- The newest deployment auto-update queued, and the digest it was for. Its outcome is read
	-- at every check, so a rollback minutes later still counts against that digest.
	deployment_id BIGINT REFERENCES deployments(id) ON DELETE SET NULL,
	deployed_digest VARCHAR(140),
	-- A digest whose auto-update failed or was rolled back: not tried again until the tag
	-- names another one.
	bad_digest VARCHAR(140),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	CONSTRAINT image_auto_update_window CHECK (
		(window_start_minute IS NULL) = (window_end_minute IS NULL)
		AND (window_start_minute IS NULL OR window_start_minute <> window_end_minute)
	)
);
CREATE INDEX image_auto_update_due ON image_auto_update (next_check_at) WHERE enabled;
