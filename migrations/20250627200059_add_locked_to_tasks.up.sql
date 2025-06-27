ALTER TABLE tasks
    ADD COLUMN locked_at TIMESTAMP NULL;

CREATE INDEX IF NOT EXISTS idx_tasks_locked_at ON tasks(locked_at);


