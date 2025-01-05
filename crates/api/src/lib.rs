use std::{env, sync::Arc};

use axum::{
  extract::{FromRequest, State},
  http::{
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
    HeaderValue, Method,
  },
  response::IntoResponse,
  routing::get,
};
use error::ApiError;
use serde_json::json;
use sqlx::SqlitePool;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_cookies::CookieManagerLayer;
use tower_http::cors::CorsLayer;
use tracing::info;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

use handlers::{projects::init_projects_routes, tasks::init_tasks_routes, users::init_users_routes};

pub mod entities;
mod error;
mod handlers;
pub mod service;
pub mod workers;

const OCTABOT_TAG: &str = "octabot";

#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
struct AppJson<T>(T);

/// Handle health check requests
async fn health_handler(State(pool): State<Arc<SqlitePool>>) -> impl IntoResponse {
  let res = sqlx::query("SELECT 1").execute(&*pool).await;
  match res {
    Ok(_) => json!({
      "code": "200",
      "success": true,
    })
    .to_string(),
    Err(_) => json!({
      "code": "500",
      "success": false,
    })
    .to_string(),
  }
}

pub async fn run(state: Arc<SqlitePool>, cancel_token: CancellationToken) -> anyhow::Result<()> {
  let host = env::var("HOST").expect("HOST is not set in .env file");
  let port = env::var("PORT").expect("PORT is not set in .env file");
  let server_url = format!("{host}:{port}");

  // Initialize cors settings
  let cors = CorsLayer::new()
    .allow_origin("http://localhost:3000".parse::<HeaderValue>()?)
    .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
    .allow_credentials(true)
    .allow_headers([AUTHORIZATION, ACCEPT, CONTENT_TYPE]);

  #[derive(OpenApi)]
  #[openapi(
    tags(
      (name = OCTABOT_TAG, description = "Bot management API")
    )
  )]
  struct ApiDoc;

  let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
    .route("/health", get(health_handler))
    .nest("/api/users", init_users_routes(state.clone()))
    .nest("/api/projects", init_projects_routes(state.clone()))
    .nest("/api/tasks", init_tasks_routes(state.clone()))
    .layer(CookieManagerLayer::new())
    .layer(cors)
    .with_state(state)
    .split_for_parts();

  let router = router.merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", api.clone()));

  info!("Starting api server...");

  let listener = TcpListener::bind(&server_url).await?;
  axum::serve(listener, router.into_make_service())
    .with_graceful_shutdown(Box::pin(async move { cancel_token.cancelled().await }))
    .await?;

  info!("Stopped api server");

  Ok(())
}
