use std::{env, sync::Arc};

use anyhow::Result;
use futures::FutureExt;
use octabot_plugins::manager::PluginManager;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, subscriber};
use tracing_subscriber::{layer::SubscriberExt, EnvFilter};

mod utils;

#[tokio::main]
async fn main() -> Result<()> {
  dotenvy::dotenv().ok();

  let log_level = env::var("TEAM_BOT_LOG_LEVEL").expect("TEAM_BOT_LOG_LEVEL is not set in .env file");
  let db_url = env::var("DATABASE_URL").expect("DATABASE_URL is not set in .env file");

  let subscriber = tracing_subscriber::registry().with(EnvFilter::from_default_env().add_directive(log_level.parse()?));

  subscriber::set_global_default(subscriber)?;

  let cancel_token = CancellationToken::new();

  // Start task for catching interrupt
  tokio::spawn({
    let cancel_token = cancel_token.clone();
    async move {
      let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
      };

      #[cfg(unix)]
      let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
          .expect("failed to install signal handler")
          .recv()
          .await;
      };

      #[cfg(not(unix))]
      let terminate = std::future::pending::<()>();

      tokio::select! {
        _ = ctrl_c => {
          info!("Received Ctrl-C, shutting down...");
          cancel_token.cancel()
        },
        _ = terminate => {
          info!("Received terminate, shutting down...");
          cancel_token.cancel()
        },
      }
    }
  });

  let pool = SqlitePoolOptions::new()
    .max_connections(100)
    .min_connections(5)
    .connect(&db_url)
    .await
    .expect("Database connection failed");

  let shared_pool = Arc::new(pool);

  let pluginManager = PluginManager::new()?;

  if let Err(err) = utils::join_all(
    vec![
      octabot_api::run(shared_pool.clone(), cancel_token.clone()).boxed(),
      // clean::run(shared_pool.clone(), cancel_token.clone()).boxed(),
    ],
    cancel_token,
  )
  .await
  {
    error!("One of main thread get error while execution: {:?}", err);
  }

  shared_pool.close().await;

  Ok(())
}