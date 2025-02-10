wasmtime::component::bindgen!({
  path: "wit/",
  async: true,
  trappable_imports: true,
});
