use serde::{Deserialize, Serialize};

use crate::manager::PluginLocation;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Plugin {
  pub source: PluginLocation,
  pub author: String,
  pub description: String,
  pub version: String,
}
