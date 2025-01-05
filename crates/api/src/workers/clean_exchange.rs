use std::sync::Arc;

use anyhow::Result;
use sqlx::SqlitePool;
use tokio::select;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::service::mutation;

static QUERY_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn run(pool: Arc<SqlitePool>, cancel_token: CancellationToken) -> Result<()> {
  info!("Cleaning exchange tasks jobs started");

  while !cancel_token.is_cancelled() {
    select! {
      biased;
      _ = cancel_token.cancelled() => {
        info!("Cleaning exchange tasks jobs stopped");
        break;
      }
      _ = sleep(QUERY_TIMEOUT) => {
        let affected_tasks = mutation::tasks::delete_by_update_date(&pool).await;

        if let Err(e) = affected_tasks {
          error!("Failed to delete old exchange tasks: {}", e);

          continue;
        }

        debug!("Delete {} old exchange tasks", affected_tasks?);
      }
    }
  }

  Ok(())
}
