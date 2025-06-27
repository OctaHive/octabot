use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use sqlx::sqlite::SqliteRow;
use sqlx::{SqlitePool, Row};
use uuid::Uuid;

use crate::{
  entities::{
    project::ProjectRow,
    task::{Task, TaskRow, TaskStatus},
  },
  error::{ApiError, ApiResult},
};

// SQL Query Constants
const INSERT_TASK: &str = r#"
  INSERT INTO tasks (id, type, project_id, name, external_id, external_modified_at, schedule, start_at, options)
  VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
  ON CONFLICT (external_id) DO UPDATE SET
    name = excluded.name,
    start_at = excluded.start_at,
    schedule = excluded.schedule,
    external_modified_at = excluded.external_modified_at,
    options = excluded.options,
    updated_at = CURRENT_TIMESTAMP
  RETURNING *
"#;

const SELECT_TASKS_TO_RUN: &str = r#"
  SELECT t.id
  FROM tasks t
  WHERE t.status NOT IN ('finished', 'in_progress')
  AND t.retries < 3
  AND t.start_at <= unixepoch()
  AND (t.locked_at IS NULL OR t.locked_at < datetime('now', '-5 minutes'))
  ORDER BY t.id
"#;

const UPDATE_TASKS_STATUS: &str = r#"
  UPDATE tasks
  SET status = 'in_progress',
    locked_at = datetime('now')
  WHERE id IN
"#;

const SELECT_TASKS_WITH_PROJECTS: &str = r#"
  SELECT
    p.id as project_id,
    p.name as project_name,
    p.code as project_code,
    p.options as project_options,
    p.owner_id as project_owner_id,
    p.created_at as project_created_at,
    p.updated_at as project_updated_at,
    t.id as task_id,
    t.type as task_type,
    t.status as task_status,
    t.options as task_options,
    t.start_at as task_start_at,
    t.schedule as task_schedule,
    t.name as task_name,
    t.retries as task_retries,
    t.external_id as task_external_id,
    t.external_modified_at as task_external_modified_at,
    t.created_at as task_created_at,
    t.updated_at as task_updated_at
  FROM tasks t
  LEFT JOIN projects p ON t.project_id = p.id
  WHERE t.id IN
"#;

const UPDATE_TASK: &str = r#"
  UPDATE tasks
  SET name = ?1, schedule = ?2, start_at = ?3, options = ?4
  WHERE id = ?5
  RETURNING *
"#;

const FIND_TASK: &str = "SELECT * FROM tasks WHERE id = ?1";
const FIND_TASK_BY_EXTERNAL_ID: &str = "SELECT * FROM tasks WHERE external_id = ?1";
const FIND_PROJECT: &str = "SELECT * FROM projects WHERE id = ?1";
const DELETE_TASK: &str = "DELETE FROM tasks WHERE id = ?";
const SCHEDULE_TASK: &str = "UPDATE tasks SET status = ?1, start_at = ?2 WHERE id = ?3 RETURNING *";
const UPDATE_TASK_STATUS: &str = "UPDATE tasks SET status = ?1 WHERE id = ?2 RETURNING *";
const DELETE_OLD_TASKS: &str = "DELETE FROM tasks WHERE status = 'finished' AND updated_at < date('now','-1 day')";
const DELETE_STALE_TASKS: &str =
  "DELETE FROM tasks WHERE external_id IS NOT NULL AND updated_at <= date('now','-10 seconds')";

#[derive(Debug, Deserialize)]
pub struct CreateTaskParams {
  pub r#type: String,
  pub name: String,
  pub project_id: Uuid,
  pub schedule: Option<String>,
  pub external_id: Option<String>,
  pub external_modified_at: Option<DateTime<Utc>>,
  pub start_at: i32,
  pub options: Value,
}

pub async fn create(pool: &SqlitePool, params: CreateTaskParams) -> ApiResult<Task> {
  let existing_task = match &params.external_id {
    Some(external_id) => get_task_by_external_id(pool, external_id).await?,
    None => None,
  };

  let task = create_task_row(pool, &params).await?;
  let project = get_project(pool, params.project_id).await?;

  if let Some(existing_task) = existing_task {
    let should_update = match (existing_task.external_modified_at, params.external_modified_at) {
      (Some(existing_modified_at), Some(task_modified_at)) => {
        is_status_update_needed(&existing_task, existing_modified_at, task_modified_at)
      },
      _ => false,
    };

    if should_update {
      update_task_status(pool, existing_task.id, TaskStatus::New).await?;
    }
  }

  Ok(build_task(task, project))
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskParams {
  pub name: String,
  pub schedule: Option<String>,
  pub start_at: i32,
  pub options: Value,
}

pub async fn update(pool: &SqlitePool, id: Uuid, params: UpdateTaskParams) -> ApiResult<Task> {
  ensure_task_exists(pool, id).await?;

  let task = update_task_row(pool, id, &params).await?;
  let project = get_project(pool, task.project_id).await?;

  Ok(build_task(task, project))
}

pub async fn run_task(pool: &SqlitePool, id: Uuid) -> ApiResult<TaskRow> {
  update_task_status(pool, id, TaskStatus::InProgress).await
}

pub async fn failed_task(pool: &SqlitePool, id: Uuid) -> ApiResult<TaskRow> {
  update_task_status(pool, id, TaskStatus::Failed).await
}

pub async fn completed_task(pool: &SqlitePool, id: Uuid) -> ApiResult<TaskRow> {
  update_task_status(pool, id, TaskStatus::Finished).await
}

pub async fn schedule_task(pool: &SqlitePool, id: Uuid, start_at: i32) -> ApiResult<TaskRow> {
  ensure_task_exists(pool, id).await?;

  sqlx::query_as::<_, TaskRow>(SCHEDULE_TASK)
    .bind(TaskStatus::New.to_string())
    .bind(start_at)
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

pub async fn delete(pool: &SqlitePool, id: Uuid) -> ApiResult<()> {
  ensure_task_exists(pool, id).await?;

  sqlx::query(DELETE_TASK).bind(id).execute(pool).await?;

  Ok(())
}

pub async fn delete_completed_tasks(pool: &SqlitePool) -> ApiResult<u64> {
  Ok(sqlx::query(DELETE_OLD_TASKS).execute(pool).await?.rows_affected())
}

pub async fn delete_by_update_date(pool: &SqlitePool) -> ApiResult<u64> {
  Ok(sqlx::query(DELETE_STALE_TASKS).execute(pool).await?.rows_affected())
}

async fn create_task_row(pool: &SqlitePool, params: &CreateTaskParams) -> ApiResult<TaskRow> {
  sqlx::query_as::<_, TaskRow>(INSERT_TASK)
    .bind(Uuid::new_v4())
    .bind(&params.r#type)
    .bind(params.project_id)
    .bind(&params.name)
    .bind(&params.external_id)
    .bind(params.external_modified_at)
    .bind(&params.schedule)
    .bind(params.start_at)
    .bind(&params.options)
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

async fn update_task_row(pool: &SqlitePool, id: Uuid, params: &UpdateTaskParams) -> ApiResult<TaskRow> {
  sqlx::query_as::<_, TaskRow>(UPDATE_TASK)
    .bind(&params.name)
    .bind(&params.schedule)
    .bind(params.start_at)
    .bind(&params.options)
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

pub async fn get_tasks_to_run(pool: &SqlitePool) -> ApiResult<Vec<Task>> {
  let mut tx = pool.begin().await?;

  let task_ids: Vec<Uuid> = sqlx::query_scalar(SELECT_TASKS_TO_RUN)
      .fetch_all(&mut *tx)
      .await?;

  if task_ids.is_empty() {
    tx.commit().await?;
    return Ok(vec![]);
  }

  // Формируем строку с плейсхолдерами для IN условия
  let placeholders = format!("({})", std::iter::repeat("?").take(task_ids.len()).collect::<Vec<_>>().join(","));

  let update_query = format!("{}{}", UPDATE_TASKS_STATUS, placeholders);
  let select_query = format!("{}{}", SELECT_TASKS_WITH_PROJECTS, placeholders);

  // Создаем запрос и привязываем каждый UUID отдельно
  let mut query = sqlx::query(&update_query);
  for id in &task_ids {
    query = query.bind(id);
  }
  query.execute(&mut *tx).await?;

  let mut query = sqlx::query(&select_query);
  for id in &task_ids {
    query = query.bind(id);
  }
  let tasks = query.map(map_task).fetch_all(&mut *tx).await?;

  tx.commit().await?;
  Ok(tasks)
}

async fn get_project(pool: &SqlitePool, project_id: Uuid) -> ApiResult<ProjectRow> {
  sqlx::query_as::<_, ProjectRow>(FIND_PROJECT)
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

async fn ensure_task_exists(pool: &SqlitePool, id: Uuid) -> ApiResult<()> {
  let exists = sqlx::query_as::<_, TaskRow>(FIND_TASK)
    .bind(id)
    .fetch_optional(pool)
    .await?;

  match exists {
    Some(_) => Ok(()),
    None => Err(ApiError::ResourceNotFound(id.to_string())),
  }
}

async fn get_task_by_external_id(pool: &SqlitePool, external_id: &str) -> ApiResult<Option<TaskRow>> {
  sqlx::query_as::<_, TaskRow>(FIND_TASK_BY_EXTERNAL_ID)
    .bind(external_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

async fn update_task_status(pool: &SqlitePool, id: Uuid, status: TaskStatus) -> ApiResult<TaskRow> {
  ensure_task_exists(pool, id).await?;

  sqlx::query_as::<_, TaskRow>(UPDATE_TASK_STATUS)
    .bind(status.to_string())
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

fn is_status_update_needed(
  existing_task: &TaskRow,
  existing_modified_at: DateTime<Utc>,
  task_modified_at: DateTime<Utc>,
) -> bool {
  existing_task
    .status
    .parse::<TaskStatus>()
    .map(|status| status == TaskStatus::Failed && task_modified_at > existing_modified_at)
    .unwrap_or(false)
}

fn build_task(task: TaskRow, project: ProjectRow) -> Task {
  Task {
    id: task.id,
    name: task.name,
    r#type: task.r#type,
    status: task.status,
    project,
    retries: task.retries,
    external_id: task.external_id,
    external_modified_at: task.external_modified_at,
    schedule: task.schedule,
    start_at: task.start_at,
    options: task.options,
    created_at: task.created_at,
    updated_at: task.updated_at,
  }
}

fn map_task(row: SqliteRow) -> Task {
  Task {
    id: row.get("task_id"),
    r#type: row.get("task_type"),
    status: row.get("task_status"),
    options: row.get("task_options"),
    start_at: row.get("task_start_at"),
    schedule: row.get("task_schedule"),
    name: row.get("task_name"),
    retries: row.get("task_retries"),
    external_id: row.get("task_external_id"),
    external_modified_at: row.get("task_external_modified_at"),
    project: map_project_row(&row),
    created_at: row.get("task_created_at"),
    updated_at: row.get("task_updated_at"),
  }
}

fn map_project_row(row: &SqliteRow) -> ProjectRow {
  ProjectRow {
    id: row.get("project_id"),
    name: row.get("project_name"),
    code: row.get("project_code"),
    options: row.get("project_options"),
    owner_id: row.get("project_owner_id"),
    created_at: row.get("project_created_at"),
    updated_at: row.get("project_updated_at"),
  }
}