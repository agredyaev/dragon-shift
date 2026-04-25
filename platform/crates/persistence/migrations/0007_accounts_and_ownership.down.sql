-- Rollback for 0007_accounts_and_ownership.sql
--
-- WARNING: DESTRUCTIVE. Running this rollback unlinks all characters from
-- their owner accounts and drops every account row. Only apply after a
-- verified backup of the `accounts` and `characters` tables.
--
-- Operational procedure:
--   1. Take a logical backup:
--        pg_dump --data-only --table=accounts --table=characters > /backup/0007-pre-rollback.sql
--   2. Run this file inside a transaction against the target database.
--   3. Roll the application image back to the pre-0007 tag in the same deploy.
--
-- sqlx does not auto-apply .down.sql files; operators execute this manually
-- via psql or through a one-off Kubernetes Job using the app container image.

BEGIN;

DROP INDEX IF EXISTS characters_owner_idx;

ALTER TABLE characters
    DROP COLUMN IF EXISTS owner_account_id;

DROP INDEX IF EXISTS accounts_name_lower_idx;

DROP TABLE IF EXISTS accounts;

COMMIT;
