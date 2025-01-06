use std::io;

use octabot_plugins::error::PluginError;

pub type ExecutorResult<T = ()> = Result<T, ExecutorError>;

#[derive(thiserror::Error, Debug)]
pub enum ExecutorError {
  #[error("Failed to open the config file: {0}")]
  ConfigOpenError(#[from] io::Error),

  #[error("Failed to read config file: {0}")]
  ConfigReadError(String),

  #[error("Ocuured plugin error: {0}")]
  PluginError(#[from] PluginError),

  #[error("Failed to parse cron schedule: {0}")]
  ParseCronError(String),

  #[error("Failed to calculate cron schedule")]
  CalculateCronScheduleError,

  #[error("Failed to parse timestamp")]
  InvalidTimestampError,

  #[error("Invalid schedule format: must start with '@every'")]
  InvalidScheduleFormat,

  #[error("Failed to parse duration: {0}")]
  DurationParseError(String),

  #[error("Failed to convert to chrono duration")]
  DurationConvertError,

  #[error("Unknown plugin type: {0}")]
  UnknownPluginError(String),
}
