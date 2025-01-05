use std::sync::Arc;

use anyhow::Result;
use wasmtime::{component::Linker, WasmBacktraceDetails};
use wasmtime_wasi_keyvalue::WasiKeyValue;

use crate::{bindings::wasi, state::State};

pub struct Config {
  inner: wasmtime::Config,
}

impl Config {}

impl Default for Config {
  fn default() -> Self {
    let mut inner = wasmtime::Config::new();
    inner.async_support(true);
    inner.wasm_component_model(true);
    inner.wasm_backtrace_details(WasmBacktraceDetails::Enable);

    Self { inner }
  }
}

pub struct EngineBuilder {
  engine: wasmtime::Engine,
  linker: Linker<State>,
}

impl EngineBuilder {
  fn new(config: &Config) -> Result<Self> {
    let engine = wasmtime::Engine::new(&config.inner)?;
    let mut linker: Linker<State> = Linker::new(&engine);

    wasmtime_wasi::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;
    wasmtime_wasi_keyvalue::add_to_linker(&mut linker, |ctx| {
      WasiKeyValue::new(&ctx.wasi_keyvalue_ctx, &mut ctx.table)
    })?;
    wasi::logging::logging::add_to_linker(&mut linker, |ctx| ctx)?;

    Ok(Self { engine, linker })
  }

  pub fn build(self) -> Engine {
    Engine {
      inner: self.engine,
      linker: Arc::new(self.linker),
    }
  }
}

#[derive(Clone)]
pub struct Engine {
  pub inner: wasmtime::Engine,
  pub linker: Arc<Linker<State>>,
}

impl AsRef<wasmtime::Engine> for Engine {
  fn as_ref(&self) -> &wasmtime::Engine {
    &self.inner
  }
}

impl Engine {
  pub fn builder(config: &Config) -> Result<EngineBuilder> {
    EngineBuilder::new(config)
  }
}
