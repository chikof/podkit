-- A deployment promoted from another environment runs the image of the deployment it came
-- from, without a build. `promoted_from` points at that deployment while it exists;
-- `promoted_from_environment` keeps the name of its environment, so the history still says
-- where a release came from after the source application is deleted.
ALTER TABLE deployments
	ADD COLUMN promoted_from BIGINT REFERENCES deployments (id) ON DELETE SET NULL,
	ADD COLUMN promoted_from_environment VARCHAR(60),
	ADD CONSTRAINT deployments_promotion_named
		CHECK (promoted_from IS NULL OR promoted_from_environment IS NOT NULL);

-- Deleting an application deletes its deployments; this keeps the SET NULL above from
-- scanning the whole table for each of them.
CREATE INDEX idx_deployments_promoted_from ON deployments (promoted_from)
	WHERE promoted_from IS NOT NULL;
