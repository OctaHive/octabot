use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use std::str::FromStr;
use tokio::{
  sync::{
    mpsc::{channel, Receiver, Sender},
    Mutex,
  },
  time::sleep,
};
use tracing::instrument;

use octabot_api::entities::task::Task;

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const CHANNEL_CAPACITY: usize = 500;

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginConfig {
  pub name: String,
  pub options: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Config {
  num_workers: u32,
  plugins: Vec<PluginConfig>,
}

impl Config {
  fn from_file(path: &str) -> Result<Self> {
    let file = std::fs::File::open(path).with_context(|| format!("Failed to open config file: {}", path))?;

    serde_json::from_reader(file).with_context(|| "Failed to parse config file")
  }
}

pub struct ExecutorSystem {
  config: Config,
  pool: Arc<SqlitePool>,
  tx: Sender<Task>,
  rx: Arc<Mutex<Receiver<Task>>>,
}

#[instrument(level = "debug")]
fn calculate_next_run(task: &Task) -> Result<i32> {
  let start_at =
    DateTime::from_timestamp(task.start_at as i64, 0).ok_or_else(|| anyhow::anyhow!("Invalid timestamp"))?;

  let next_run = if let Some(schedule) = &task.schedule {
    if schedule.starts_with("@every") {
      calculate_interval_next_run(schedule, start_at)?
    } else {
      calculate_cron_next_run(schedule, start_at)?
    }
  } else {
    start_at.timestamp() as i32
  };

  Ok(next_run.max(Utc::now().timestamp() as i32))
}

fn calculate_interval_next_run(schedule: &str, start_at: DateTime<Utc>) -> Result<i32> {
  // Extract interval duration from schedule string
  let duration_str = schedule
    .strip_prefix("@every ")
    .ok_or_else(|| anyhow!("Invalid schedule format: must start with '@every'"))?;

  // Parse duration string into std::time::Duration
  let std_duration = duration_str::parse(duration_str).map_err(|e| anyhow!("Failed to parse duration: {}", e))?;

  // Convert to chrono::Duration
  let interval = chrono::Duration::from_std(std_duration).context("Failed to convert to chrono duration")?;

  // Calculate timestamps
  let current_time = Utc::now().timestamp();
  let start_time = start_at.timestamp();
  let interval_seconds = interval.num_seconds();

  // Ensure interval_seconds is not zero to avoid division by zero
  if interval_seconds == 0 {
    return Err(anyhow!("Interval duration cannot be zero"));
  }

  // Calculate number of intervals passed since start
  let intervals_passed = (current_time - start_time) / interval_seconds + 1;

  // Calculate next run timestamp
  let next_run = start_time + (intervals_passed * interval_seconds);

  // Convert to i32, checking for overflow
  next_run.try_into().context("Next run timestamp exceeds i32 range")
}

fn calculate_cron_next_run(schedule: &str, start_at: DateTime<Utc>) -> Result<i32> {
  let schedule = Schedule::from_str(schedule).context("Failed to parse cron schedule")?;

  let next_run = schedule
    .after(&start_at)
    .next()
    .ok_or_else(|| anyhow::anyhow!("Failed to calculate next cron run"))?;

  Ok(next_run.timestamp() as i32)
}
