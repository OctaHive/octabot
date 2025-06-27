DROP INDEX IF EXISTS idx_tasks_locked_at;
ALTER TABLE tasks DROP COLUMN locked_at;


