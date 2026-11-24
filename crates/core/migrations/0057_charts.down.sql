ALTER TABLE metric_alerts ALTER COLUMN metric TYPE VARCHAR(10);
DROP INDEX idx_model_usage_model_hour;
DROP TABLE container_events;
DROP TABLE resource_extra_metrics;
