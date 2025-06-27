#![allow(deprecated)]
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use cron::Schedule;
use octabot_plugins::{
  bindings::exports::octahive::octabot::plugin::PluginResult,
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
  entities::{project::ProjectRow, task::Task},
  service::{mutation, query},
};

use crate::error::{ExecutorError, ExecutorResult};

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const CHANNEL_CAPACITY: usize = 500;

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginConfig {
  pub name: String,
  pub path: String,
  pub options: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExecuteParams {
  task_id: String,
  options: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct Config {
  num_workers: u32,
  plugins: Vec<PluginConfig>,
}

impl Config {
  fn from_file(path: &str) -> ExecutorResult<Self> {
    let file = std::fs::File::open(path).map_err(ExecutorError::ConfigOpenError)?;

    serde_json::from_reader(file).map_err(|e| ExecutorError::ConfigReadError(e.to_string()))
  }
}

pub struct Plugin {
  pub instance: InstanceData,
  pub store: Arc<Mutex<Store<State>>>,
  pub options: Option<Value>,
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
  pub async fn new(pool: Arc<SqlitePool>) -> ExecutorResult<Self> {
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

  async fn initialize_plugins(configs: &[PluginConfig]) -> ExecutorResult<HashMap<String, Plugin>> {
    let mut plugins = HashMap::new();
    let plugin_manager = PluginManager::new()?;

    for config in configs {
      let options = config.options.clone().unwrap_or_default();
      let (instance, store) = plugin_manager.load_plugin(&config.path).await?;
      let store = Arc::new(Mutex::new(store));

      let mut store_guard = store.lock().await;
      let plugin = match instance.init(&mut store_guard, &options.to_string()).await {
        Ok(_) => {
          info!("Plugin {} initialized successfully", instance.metadata.name);
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
          options: config.options.clone(),
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

            match mutation::tasks::get_tasks_to_run(&pool).await {
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
    let execute_params = ExecuteParams {
      task_id: task.id.to_string(),
      options: serde_json::to_value(&task.options)?,
    };

    // Call process_action instead of directly working with plugin
    match Self::process_action(pool, plugins, task.r#type.clone(), &execute_params).await {
      Ok(_) => {
        if task.schedule.is_some() {
          let start_at = calculate_next_run(&task).context("Failed to calculate next run time")?;

          mutation::tasks::schedule_task(pool, task.id, start_at)
            .await
            .context("Failed to schedule next task run")?;
        } else {
          mutation::tasks::completed_task(pool, task.id)
            .await
            .context("Failed to mark task as completed")?;
        }
        Ok(())
      },
      Err(e) => {
        error!("Task execution failed: {}", e);
        mutation::tasks::failed_task(pool, task.id)
          .await
          .context("Failed to mark task as failed")?;
        Err(e)
      },
    }
  }

  fn process_action<'a>(
    pool: &'a SqlitePool,
    plugins: &'a HashMap<String, Plugin>,
    action_type: String,
    action: &'a ExecuteParams,
  ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
      let plugin = plugins
        .get(&action_type)
        .ok_or_else(|| ExecutorError::UnknownPluginError(action_type))?;

      let results = {
        let mut store = plugin.store.lock().await;
        let action_str = serde_json::to_string(action).context("Failed to serialize action params")?;

        plugin.instance.process(&mut store, &action_str).await?
      };

      for result in results {
        match result {
          PluginResult::Action(action) => {
            let params: ExecuteParams =
              serde_json::from_str(&action.payload).context("Failed to deserialize action payload")?;
            Self::process_action(pool, plugins, action.name, &params).await?;
          },
          PluginResult::Task(task) => {
            let projects = query::projects::list_all(pool).await?;
            let projects = projects
              .into_iter()
              .map(|p| (p.code.clone(), p))
              .collect::<HashMap<String, ProjectRow>>();
            let project = projects
              .get(&task.project_code)
              .context(format!("Project {} not found", task.project_code))?;

            let naive = NaiveDateTime::from_timestamp(task.external_modified_at as i64, 0);
            let external_modified_at: DateTime<Utc> = DateTime::<Utc>::from_utc(naive, Utc);

            let task_params = mutation::tasks::CreateTaskParams {
              name: task.name,
              r#type: task.kind,
              schedule: None,
              project_id: project.id,
              external_id: Some(task.external_id),
              external_modified_at: Some(external_modified_at.to_utc()),
              start_at: task.start_at as i32,
              options: serde_json::to_value(task.options).context("Failed to parse task options")?,
            };

            mutation::tasks::create(pool, task_params).await?;
          },
        }
      }

      Ok(())
    })
  }
}

#[instrument(level = "debug")]
fn calculate_next_run(task: &Task) -> Result<i32> {
  let start_at =
    DateTime::from_timestamp(task.start_at as i64, 0).ok_or_else(|| ExecutorError::InvalidTimestampError)?;

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
    .ok_or_else(|| ExecutorError::InvalidScheduleFormat)?;

  // Parse duration string into std::time::Duration
  let std_duration = duration_str::parse(duration_str).map_err(|e| ExecutorError::DurationParseError(e.to_string()))?;

  // Convert to chrono::Duration
  let interval = chrono::Duration::from_std(std_duration).map_err(|_| ExecutorError::DurationConvertError)?;

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
  let schedule = Schedule::from_str(schedule).map_err(|e| ExecutorError::ParseCronError(e.to_string()))?;

  let next_run = schedule
    .after(&start_at)
    .next()
    .ok_or_else(|| ExecutorError::CalculateCronScheduleError)?;

  Ok(next_run.timestamp() as i32)
}
