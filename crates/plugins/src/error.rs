use crate::bindings::exports::octahive::octabot::plugin::Error as WitError;

pub type PluginResult<T = ()> = Result<T, PluginError>;

#[derive(thiserror::Error, Debug)]
pub enum PluginError {
  #[error("Failed to read plugin: {0}")]
  PluginReadError(String),

  #[error("Failed to initialize wasmtime engine: {0}")]
  InitWasmEngineError(String),

  #[error("Failed to read component: {0}")]
  ReadComponentError(String),

  #[error("Failed to initialize component: {0}")]
  InitComponentError(String),

  #[error("Parse bot config error: {0}")]
  ParseBotConfigError(String),

  #[error("Parse action payload error: {0}")]
  ParseActionPaylodError(String),

  #[error("Send http request error: {0}")]
  SendHttpRequestError(String),

  #[error("Parse response error: {0}")]
  ParseResponseError(String),

  #[error("Can't open keyvalue storage: {0}")]
  OpenStorageError(String),

  #[error("Storage operation error: {0}")]
  StorageOperationError(#[from] crate::bindings::wasi::keyvalue::store::Error),

  #[error("Plugin config lock error: {0}")]
  ConfigLockError(String),

  #[error("Unexpected error: {0}")]
  OtherError(String),

  #[error("Error to calling plugin api: {0}")]
  CallPluginError(String),
}

impl From<WitError> for PluginError {
  fn from(error: WitError) -> PluginError {
    match error {
      WitError::Other(s) => PluginError::OtherError(s),
      WitError::ParseBotConfig(e) => PluginError::ParseBotConfigError(e.to_string()),
      WitError::ParseActionPayload(e) => PluginError::ParseActionPaylodError(e.to_string()),
      WitError::SendHttpRequest(e) => PluginError::SendHttpRequestError(e.to_string()),
      WitError::ParseResponse(e) => PluginError::ParseResponseError(e.to_string()),
      WitError::OpenStorage(e) => PluginError::OpenStorageError(e.to_string()),
      WitError::ConfigLock(e) => PluginError::ConfigLockError(e.to_string()),
      WitError::StorageOperation(e) => {
        PluginError::StorageOperationError(crate::bindings::wasi::keyvalue::store::Error::Other(e))
      },
    }
  }
}
