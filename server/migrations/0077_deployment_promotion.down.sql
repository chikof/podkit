DROP INDEX IF EXISTS idx_deployments_promoted_from;
ALTER TABLE deployments
	DROP CONSTRAINT IF EXISTS deployments_promotion_named,
	DROP COLUMN IF EXISTS promoted_from_environment,
	DROP COLUMN IF EXISTS promoted_from;
