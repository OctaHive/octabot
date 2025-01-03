use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use octabot_plugins::{
  manager::{InstanceData, PluginActions, PluginManager},
  state::State,
};
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
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument};
use wasmtime::Store;

use octabot_api::{
  entities::task::Task,
  service::{mutation, query},
};

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const CHANNEL_CAPACITY: usize = 500;

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginConfig {
  pub name: String,
  pub path: String,
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

pub struct Plugin {
  pub instance: InstanceData,
  pub store: Arc<Mutex<Store<State>>>,
}

pub struct ExecutorSystem {
  config: Config,
  pool: Arc<SqlitePool>,
  plugins: Arc<HashMap<String, Plugin>>,
  tx: Sender<Task>,
  rx: Arc<Mutex<Receiver<Task>>>,
}

impl ExecutorSystem {
  #[instrument(level = "debug", skip(pool))]
  pub async fn new(pool: Arc<SqlitePool>) -> Result<Self> {
    let (tx, rx) = channel::<Task>(CHANNEL_CAPACITY);

    let config = Config::from_file("config.json")?;
    let plugins = Self::initialize_plugins(&config.plugins).await?;

    Ok(Self {
      config,
      pool,
      plugins: Arc::new(plugins),
      tx,
      rx: Arc::new(Mutex::new(rx)),
    })
  }

  async fn initialize_plugins(configs: &[PluginConfig]) -> Result<HashMap<String, Plugin>> {
    let mut plugins = HashMap::new();
    let plugin_manager = PluginManager::new()?;

    for config in configs {
      let (instance, store) = plugin_manager.load_plugin(&config.path).await?;
      let store = Arc::new(Mutex::new(store));

      let mut store_guard = store.lock().await;
      let plugin = match instance.init(&mut store_guard).await {
        Ok(metadata) => {
          info!("Plugin {} initialized successfully", metadata.name);
          instance
        },
        Err(e) => {
          error!("Failed to initialize plugin {}: {}", config.name, e);
          continue;
        },
      };
      drop(store_guard);

      plugins.insert(
        config.name.clone(),
        Plugin {
          instance: plugin,
          store,
        },
      );
    }

    Ok(plugins)
  }

  #[instrument(level = "debug", skip(self, cancel_token))]
  pub async fn run(self, cancel_token: CancellationToken) -> Result<()> {
    let mut handlers = vec![];
    info!("Starting executor...");

    handlers.push(self.spawn_task_poller(cancel_token.clone()));
    handlers.extend(self.spawn_workers(cancel_token));

    info!("Executor started");

    futures::future::join_all(handlers).await;
    info!("Executor system stopped");

    Ok(())
  }

  fn spawn_task_poller(&self, cancel_token: CancellationToken) -> tokio::task::JoinHandle<()> {
    let pool = self.pool.clone();
    let tx = self.tx.clone();

    tokio::spawn(async move {
      info!("Task poller started");

      while !cancel_token.is_cancelled() {
        tokio::select! {
          _ = sleep(QUERY_TIMEOUT) => {
            debug!("Start polling task from db...");

            match query::tasks::get_tasks_to_run(&pool).await {
              Ok(tasks) => {
                debug!("Found {} tasks to run", tasks.len());
                for task in tasks {
                  if let Err(e) = tx.send(task).await {
                    error!("Failed to send task to executor: {}", e);
                  }
                }
              },
              Err(e) => error!("Failed to get tasks to run: {}", e),
            }
          }
          _ = cancel_token.cancelled() => {
            info!("Task poller stopped");
            break;
          }
        }
      }
    })
  }

  fn spawn_workers(&self, cancel_token: CancellationToken) -> Vec<tokio::task::JoinHandle<()>> {
    info!("Starting {} workers...", self.config.num_workers);

    let handlers = (0..self.config.num_workers)
      .map(|id| self.spawn_worker(id, cancel_token.clone()))
      .collect();

    info!("Workers started");

    handlers
  }

  #[instrument(level = "debug", skip(self, cancel_token))]
  fn spawn_worker(&self, id: u32, cancel_token: CancellationToken) -> tokio::task::JoinHandle<()> {
    let rx = Arc::clone(&self.rx);
    let plugins = self.plugins.clone();
    let pool = self.pool.clone();

    tokio::spawn(async move {
      loop {
        let mut rx = rx.lock().await;

        tokio::select! {
          Some(task) = rx.recv() => {
            debug!("Worker {} received task {:?}", id, task);

            if let Err(e) = Self::process_task(&pool, &plugins, task).await {
              error!("Worker {} failed to process task: {}", id, e);
            }
          }
          _ = cancel_token.cancelled() => {
            info!("Worker {} stopped", id);
            break;
          }
        }
      }
    })
  }

  #[instrument(level = "debug", skip(pool, plugins))]
  async fn process_task(pool: &SqlitePool, plugins: &HashMap<String, Plugin>, task: Task) -> Result<()> {
    mutation::tasks::run_task(pool, task.id)
      .await
      .context("Failed to set task status to in_progress")?;

    let plugin = plugins
      .get(&task.r#type)
      .ok_or_else(|| anyhow::anyhow!("Unknown plugin type: {}", task.r#type))?;
    let mut store = plugin.store.lock().await;

    match plugin.instance.process(&mut store, "", "").await {
      Ok(_) => {
        if let Some(_) = &task.schedule {
          let start_at = calculate_next_run(&task).context("Failed to calculate next run time")?;

          mutation::tasks::schedule_task(pool, task.id, start_at)
            .await
            .context("Failed to schedule next task run")?;
        } else {
          mutation::tasks::completed_task(pool, task.id)
            .await
            .context("Failed to mark task as completed")?;
        }
      },
      Err(e) => {
        error!("Task execution failed: {}", e);
        mutation::tasks::failed_task(pool, task.id)
          .await
          .context("Failed to mark task as failed")?;
      },
    }

    Ok(())
  }
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
