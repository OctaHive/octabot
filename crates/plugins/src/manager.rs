use std::{
  fmt,
  path::{Path, PathBuf},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use wasmtime::{component::Component, Store};

use crate::{
  bindings::{
    exports::octahive::octabot::plugin::{Metadata, PluginResult as Result},
    Octabot,
  },
  engine::{Config, Engine},
  error::{PluginError, PluginResult},
  state::State,
};

#[async_trait]
pub trait PluginActions: Send + 'static {
  async fn load(&self, store: &mut Store<State>) -> PluginResult<Metadata>;

  async fn init(&self, store: &mut Store<State>, config: &str) -> PluginResult<()>;

  async fn process(&self, store: &mut Store<State>, params: &str) -> PluginResult<Vec<Result>>;
}

pub struct InstanceData {
  interface: Octabot,
  pub metadata: Metadata,
}

#[async_trait]
impl PluginActions for InstanceData {
  async fn load(&self, store: &mut Store<State>) -> PluginResult<Metadata> {
    self
      .interface
      .octahive_octabot_plugin()
      .call_load(store)
      .await
      .map_err(|e| PluginError::CallPluginError(e.to_string()))
  }

  async fn init(&self, store: &mut Store<State>, config: &str) -> PluginResult<()> {
    Ok(
      self
        .interface
        .octahive_octabot_plugin()
        .call_init(store, config)
        .await
        .map_err(|e| PluginError::CallPluginError(e.to_string()))??,
    )
  }

  async fn process(&self, store: &mut Store<State>, params: &str) -> PluginResult<Vec<Result>> {
    Ok(
      self
        .interface
        .octahive_octabot_plugin()
        .call_process(store, params)
        .await
        .map_err(|e| PluginError::CallPluginError(e.to_string()))??,
    )
  }
}

pub const PLUGINS_PATH: &str = "./plugins";

pub struct PluginManager {
  engine: Engine,
}

impl PluginManager {
  pub fn new() -> PluginResult<Self> {
    let config = Config::default();

    let engine = Engine::builder(&config)
      .map_err(|e| PluginError::InitWasmEngineError(e.to_string()))?
      .build();

    Ok(Self { engine })
  }

  pub async fn load_plugin(&self, path: impl AsRef<Path>) -> PluginResult<(InstanceData, Store<State>)> {
    let path = PathBuf::from(PLUGINS_PATH).join(path);
    let component =
      Component::from_file(&self.engine.inner, path).map_err(|e| PluginError::ReadComponentError(e.to_string()))?;

    let mut store = wasmtime::Store::new(&self.engine.inner, State::default());

    let interface = Octabot::instantiate_async(&mut store, &component, &self.engine.linker)
      .await
      .map_err(|e| PluginError::InitComponentError(e.to_string()))?;

    let metadata = interface
      .octahive_octabot_plugin()
      .call_load(&mut store)
      .await
      .map_err(|e| PluginError::CallPluginError(e.to_string()))?;

    Ok((
      InstanceData {
        interface,
        metadata: metadata.clone(),
      },
      store,
    ))
  }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", content = "location")]
pub enum PluginLocation {
  Local(PathBuf),
}

impl fmt::Display for PluginLocation {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    match self {
      PluginLocation::Local(path) => write!(f, "source: {}", path.to_str().unwrap()),
    }
  }
}

impl Default for PluginLocation {
  fn default() -> Self {
    PluginLocation::Local(PathBuf::new())
  }
}

impl PluginLocation {
  pub async fn load(&self) -> PluginResult<Vec<u8>> {
    match &self {
      Self::Local(path) => tokio::fs::read(path)
        .await
        .map_err(|e| PluginError::PluginReadError(e.to_string())),
    }
  }
}
