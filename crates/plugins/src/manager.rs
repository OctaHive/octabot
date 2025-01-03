use std::{
  fmt,
  path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use exports::octahive::octabot::plugin::Metadata;
use serde::{Deserialize, Serialize};
use wasmtime::{component::Component, Store};

use crate::{
  engine::{Config, Engine},
  state::State,
};

wasmtime::component::bindgen!({
  path: "wit/",
  async: true,
  trappable_imports: true,
});

pub struct InstanceData {
  interface: Octabot,
  pub metadata: Metadata,
}

pub const PLUGINS_PATH: &str = "./plugins";

#[async_trait]
pub trait PluginActions: Send + 'static {
  async fn init(&self, store: &mut Store<State>) -> Result<Metadata>;
}

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

    let metadata = interface.octahive_octabot_plugin().call_init(&mut store).await?;

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
