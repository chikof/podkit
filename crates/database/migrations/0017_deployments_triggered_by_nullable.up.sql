-- Webhook-triggered deployments have no acting user.
ALTER TABLE deployments ALTER COLUMN triggered_by DROP NOT NULL;
