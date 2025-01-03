use axum::extract::rejection::JsonRejection;
use axum::response::{IntoResponse, Response};
use axum::{http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use sqlx::Error as SqlxError;
use thiserror::Error;

pub type ApiResult<T = ()> = Result<T, ApiError>;

#[derive(Debug, Error)]
pub enum ApiError {
  #[error("Invalid credentials")]
  InvalidCredentials(),
  #[error("User with email `{0}` already exists")]
  UserAlreadyExist(String),
  #[error("Entity `{0}` is not found")]
  ResourceNotFound(String),
  #[error("Database error: {0}")]
  DatabaseError(#[from] SqlxError),
  #[error(transparent)]
  JsonRejection(JsonRejection),
  #[error(transparent)]
  InvalidInputError(#[from] validator::ValidationErrors),
  #[error("Invalid schedule format: {0}")]
  InvalidSchedule(String),
  #[error("Failed to calculate next run time: {0}")]
  ScheduleCalculation(String),
  #[error("an internal server error occurred")]
  Anyhow(#[from] anyhow::Error),
}

impl ApiError {
  pub fn response(self) -> (StatusCode, AppResponseError) {
    use ApiError::*;
    let message = self.to_string();

    let (kind, code, details, status_code) = match self {
      JsonRejection(rejection) => (
        "INVALID_INPUT_ERROR".to_string(),
        None,
        vec![(rejection.status().to_string(), vec![rejection.body_text()])],
        StatusCode::BAD_REQUEST,
      ),
      InvalidInputError(err) => (
        "INVALID_INPUT_ERROR".to_string(),
        None,
        err
          .field_errors()
          .into_iter()
          .map(|(p, e)| {
            (
              p.to_string(),
              e.iter().map(|err| err.code.to_string()).collect::<Vec<String>>(),
            )
          })
          .collect(),
        StatusCode::BAD_REQUEST,
      ),
      InvalidSchedule(_) => (
        "INTERNAL_SERVER_ERROR".to_string(),
        None,
        vec![],
        StatusCode::INTERNAL_SERVER_ERROR,
      ),
      ScheduleCalculation(_) => (
        "INTERNAL_SERVER_ERROR".to_string(),
        None,
        vec![],
        StatusCode::INTERNAL_SERVER_ERROR,
      ),
      Anyhow(ref e) => {
        tracing::error!("Generic error: {:?}", e);

        (
          "INTERNAL_SERVER_ERROR".to_string(),
          None,
          vec![],
          StatusCode::INTERNAL_SERVER_ERROR,
        )
      },
      DatabaseError(error) => todo!(),
      UserAlreadyExist(_) => todo!(),
      ResourceNotFound(_) => ("RESOURCE_NOT_FOUND".to_string(), None, vec![], StatusCode::NOT_FOUND),
      InvalidCredentials() => (
        "INVALID_CREDENTIALS".to_string(),
        None,
        vec![],
        StatusCode::UNAUTHORIZED,
      ),
      Anyhow(ref e) => {
        tracing::error!("Generic error: {:?}", e);

        (
          "INTERNAL_SERVER_ERROR".to_string(),
          None,
          vec![],
          StatusCode::INTERNAL_SERVER_ERROR,
        )
      },
    };

    (status_code, AppResponseError::new(kind, message, code, details))
  }
}

impl IntoResponse for ApiError {
  fn into_response(self) -> Response {
    let (status_code, body) = self.response();
    (status_code, Json(body)).into_response()
  }
}

impl From<JsonRejection> for ApiError {
  fn from(rejection: JsonRejection) -> Self {
    Self::JsonRejection(rejection)
  }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AppResponseError {
  pub kind: String,
  pub error_message: String,
  pub code: Option<i32>,
  pub details: Vec<(String, Vec<String>)>,
}

impl AppResponseError {
  pub fn new(
    kind: impl Into<String>,
    message: impl Into<String>,
    code: Option<i32>,
    details: Vec<(String, Vec<String>)>,
  ) -> Self {
    Self {
      kind: kind.into(),
      error_message: message.into(),
      code,
      details,
    }
  }
}
