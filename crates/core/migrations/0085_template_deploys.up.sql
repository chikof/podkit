-- Which catalog template an application or a stack was deployed from, and at which
-- revision of it (the `revision` of the catalog's index: it changes only when what gets
-- deployed changes). The gallery compares it with the catalog's to say that a newer
-- template exists. An imported template file has no row: it is nobody's to update.
CREATE TABLE template_deploys (
	application_id BIGINT UNIQUE REFERENCES applications(id) ON DELETE CASCADE,
	stack_id BIGINT UNIQUE REFERENCES stacks(id) ON DELETE CASCADE,
	template_id VARCHAR(80) NOT NULL,
	revision VARCHAR(32) NOT NULL,
	CHECK (num_nonnulls(application_id, stack_id) = 1)
);
