use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiView};
use wasmtime_wasi_http::{
  body::HyperOutgoingBody,
  types::{default_send_request, HostFutureIncomingResponse, OutgoingRequestConfig},
  WasiHttpCtx, WasiHttpView,
};

pub struct State {
  pub table: ResourceTable,
  pub ctx: WasiCtx,
  pub http: WasiHttpCtx,
}

impl State {
  pub fn new() -> Self {
    let mut builder = WasiCtxBuilder::new();
    builder.inherit_stdio();

    Self {
      table: ResourceTable::new(),
      ctx: builder.build(),
      http: WasiHttpCtx::new(),
    }
  }
}

impl Default for State {
  fn default() -> Self {
    Self::new()
  }
}

impl WasiView for State {
  fn table(&mut self) -> &mut ResourceTable {
    &mut self.table
  }

  fn ctx(&mut self) -> &mut WasiCtx {
    &mut self.ctx
  }
}

impl WasiHttpView for State {
  fn ctx(&mut self) -> &mut WasiHttpCtx {
    &mut self.http
  }

  fn table(&mut self) -> &mut ResourceTable {
    &mut self.table
  }

  fn send_request(
    &mut self,
    request: hyper::Request<HyperOutgoingBody>,
    config: OutgoingRequestConfig,
  ) -> wasmtime_wasi_http::HttpResult<HostFutureIncomingResponse>
  where
    Self: Sized,
  {
    Ok(default_send_request(request, config))
  }
}
