use std::{
  fmt,
  path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use wasmtime::{component::Component, Store};

use crate::{
  bindings::{
    exports::octahive::octabot::plugin::{Metadata, PluginResult},
    Octabot,
  },
  engine::{Config, Engine},
  error::PluginError,
  state::State,
};

#[async_trait]
pub trait PluginActions: Send + 'static {
  async fn load(&self, store: &mut Store<State>) -> Result<Metadata>;

  async fn init(&self, store: &mut Store<State>, config: &str) -> Result<(), PluginError>;

  async fn process(&self, store: &mut Store<State>, params: &str) -> Result<Vec<PluginResult>, PluginError>;
}

pub struct InstanceData {
  interface: Octabot,
  pub metadata: Metadata,
}

#[async_trait]
impl PluginActions for InstanceData {
  async fn load(&self, store: &mut Store<State>) -> Result<Metadata> {
    self.interface.octahive_octabot_plugin().call_load(store).await
  }

  async fn init(&self, store: &mut Store<State>, config: &str) -> Result<(), PluginError> {
    Ok(
      self
        .interface
        .octahive_octabot_plugin()
        .call_init(store, config)
        .await
        .map_err(|e| PluginError::CallPlugin(e.to_string()))??,
    )
  }

  async fn process(&self, store: &mut Store<State>, params: &str) -> Result<Vec<PluginResult>, PluginError> {
    Ok(
      self
        .interface
        .octahive_octabot_plugin()
        .call_process(store, params)
        .await
        .map_err(|e| PluginError::CallPlugin(e.to_string()))??,
    )
  }
}

pub const PLUGINS_PATH: &str = "./plugins";

pub struct PluginManager {
  engine: Engine,
}

impl PluginManager {
  pub fn new() -> Result<Self> {
    let config = Config::default();

    let engine = Engine::builder(&config)?.build();

    Ok(Self { engine })
  }

  pub async fn load_plugin(&self, path: impl AsRef<Path>) -> Result<(InstanceData, Store<State>)> {
    let path = PathBuf::from(PLUGINS_PATH).join(path);
    let component = Component::from_file(&self.engine.inner, path)?;

    let mut store = wasmtime::Store::new(&self.engine.inner, State::default());

    let interface = Octabot::instantiate_async(&mut store, &component, &self.engine.linker).await?;

    let metadata = interface.octahive_octabot_plugin().call_load(&mut store).await?;

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
  pub async fn load(&self) -> Result<Vec<u8>> {
    match &self {
      Self::Local(path) => tokio::fs::read(path)
        .await
        .map_err(|e| anyhow!("reading plugin file: {}", e)),
    }
  }
}
