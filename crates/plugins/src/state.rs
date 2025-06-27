use std::time::Instant;
use std::{collections::HashMap, sync::Arc};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::BodyExt;
use http_body_util::Empty;
use hyper::{
  client::conn::http1::SendRequest,
  header::{self, HeaderValue},
};
use lazy_static::lazy_static;
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::timeout;
use tokio::{net::TcpStream, time::sleep};
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{
  p2::{IoView, WasiCtx, WasiCtxBuilder, WasiView},
  runtime::AbortOnDropJoinHandle,
};
use wasmtime_wasi_http::{
  bindings::http::types::ErrorCode,
  body::HyperOutgoingBody,
  hyper_request_error,
  io::TokioIo,
  types::{HostFutureIncomingResponse, IncomingResponse, OutgoingRequestConfig},
  WasiHttpCtx, WasiHttpView,
};

use crate::{
  bindings::wasi,
  keyvalue::{WasiKeyValueCtx, WasiKeyValueCtxBuilder},
};

lazy_static! {
  static ref HTTP_POOL: Arc<HttpConnectionPool> = Arc::new(HttpConnectionPool::new(50));
}

#[derive(Clone)]
struct HttpConnectionPool {
  connections: Arc<Mutex<HashMap<String, Vec<PooledConnection>>>>,
  semaphore: Arc<Semaphore>,
}

struct PooledConnection {
  sender: SendRequest<HyperOutgoingBody>,
  last_used: Instant,
  created_at: Instant,
}

pub(crate) fn dns_error(rcode: String, info_code: u16) -> ErrorCode {
  ErrorCode::DnsError(wasmtime_wasi_http::bindings::http::types::DnsErrorPayload {
    rcode: Some(rcode),
    info_code: Some(info_code),
  })
}

impl HttpConnectionPool {
  const MAX_RETRIES: u32 = 2;
  const MAX_CONNECTION_AGE: Duration = Duration::from_secs(300); // 5 minutes

  pub fn new(max_connections: usize) -> Self {
    Self {
      connections: Arc::new(Mutex::new(HashMap::new())),
      semaphore: Arc::new(Semaphore::new(max_connections)),
    }
  }

  async fn get_connection(
    &self,
    authority: &str,
    use_tls: bool,
    connect_timeout: Duration,
  ) -> Result<(SendRequest<HyperOutgoingBody>, Option<AbortOnDropJoinHandle<()>>), ErrorCode> {
    let _permit = self.semaphore.acquire().await.unwrap();

    // Try to get an existing connection
    let mut connections = self.connections.lock().await;
    if let Some(connection_list) = connections.get_mut(authority) {
      while let Some(conn) = connection_list.pop() {
        // Check both idle timeout and total age
        if conn.last_used.elapsed() < Duration::from_secs(60)
          && conn.created_at.elapsed() < Self::MAX_CONNECTION_AGE
          && conn.sender.is_ready()
        {
          return Ok((conn.sender, None));
        }
        // If connection is too old, let it drop and create a new one
      }
    }

    // Create new connection if none available
    self.create_connection(authority, use_tls, connect_timeout).await
  }

  async fn create_connection(
    &self,
    authority: &str,
    use_tls: bool,
    connect_timeout: Duration,
  ) -> Result<(SendRequest<HyperOutgoingBody>, Option<AbortOnDropJoinHandle<()>>), ErrorCode> {
    let tcp_stream = TcpStream::connect(authority)
      .await
      .map_err(|_| ErrorCode::ConnectionRefused)?;

    if use_tls {
      #[cfg(not(any(target_arch = "riscv64", target_arch = "s390x")))]
      {
        use rustls::{pki_types::ServerName, RootCertStore};

        let mut root_cert_store = RootCertStore::empty();

        // Читаем сертификаты из директории certs
        tracing::info!("Loading custom certificates from certs directory");
        if let Ok(entries) = std::fs::read_dir("certs") {
          for entry in entries {
            if let Ok(entry) = entry {
              let path = entry.path();
              if path.is_file() {
                tracing::debug!("Reading certificate file: {:?}", path);
                if let Ok(cert_data) = std::fs::read(&path) {
                  // Пытаемся распарсить как PEM
                  let mut cert_slice = cert_data.as_slice();
                  let pem_certs = rustls_pemfile::certs(&mut cert_slice);
                  let mut found_pem = false;
                  for cert_result in pem_certs {
                    if let Ok(cert) = cert_result {
                      let _ = root_cert_store.add(cert);
                      found_pem = true;
                      tracing::debug!("Successfully loaded PEM certificate from {:?}", path);
                    }
                  }
                  if !found_pem {
                    // Пытаемся добавить как DER
                    let cert = rustls::pki_types::CertificateDer::from(cert_data);
                    let _ = root_cert_store.add(cert);
                    tracing::debug!("Successfully loaded DER certificate from {:?}", path);
                  }
                } else {
                  tracing::warn!("Failed to read certificate file: {:?}", path);
                }
              }
            }
          }
        } else {
          tracing::debug!("No certs directory found or unable to read it");
        }

        // Добавляем стандартные корневые сертификаты
        root_cert_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        tracing::info!("Loaded {} root certificates total", root_cert_store.len());
        let config = rustls::ClientConfig::builder()
          .with_root_certificates(root_cert_store)
          .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));
        let mut parts = authority.split(':');
        let host = parts.next().unwrap_or(authority);
        let domain = ServerName::try_from(host)
          .map_err(|_| dns_error("invalid dns name".to_string(), 0))?
          .to_owned();

        let stream = connector
          .connect(domain, tcp_stream)
          .await
          .map_err(|_| ErrorCode::TlsProtocolError)?;
        let io = TokioIo::new(stream);

        let (sender, conn) = timeout(connect_timeout, hyper::client::conn::http1::handshake(io))
          .await
          .map_err(|_| ErrorCode::ConnectionTimeout)?
          .map_err(hyper_request_error)?;

        let worker = wasmtime_wasi::runtime::spawn(async move {
          if let Err(e) = conn.await {
            tracing::warn!("connection error: {}", e);
          }
        });

        Ok((sender, Some(worker)))
      }
      #[cfg(any(target_arch = "riscv64", target_arch = "s390x"))]
      {
        Err(ErrorCode::InternalError(Some(
          "unsupported architecture for SSL".to_string(),
        )))
      }
    } else {
      let io = TokioIo::new(tcp_stream);
      let (sender, conn) = timeout(connect_timeout, hyper::client::conn::http1::handshake(io))
        .await
        .map_err(|_| ErrorCode::ConnectionTimeout)?
        .map_err(hyper_request_error)?;

      let worker = wasmtime_wasi::runtime::spawn(async move {
        if let Err(e) = conn.await {
          tracing::warn!("connection error: {}", e);
        }
      });

      Ok((sender, Some(worker)))
    }
  }

  async fn return_connection(&self, authority: String, sender: SendRequest<HyperOutgoingBody>) {
    let conn = PooledConnection {
      sender,
      last_used: Instant::now(),
      created_at: Instant::now(),
    };

    let mut connections = self.connections.lock().await;
    connections.entry(authority).or_insert_with(Vec::new).push(conn);
    self.semaphore.add_permits(1);
  }
}

pub struct State {
  pub table: ResourceTable,
  pub ctx: WasiCtx,
  pub http: WasiHttpCtx,
  pub wasi_keyvalue_ctx: WasiKeyValueCtx,
}

impl State {
  pub fn new() -> Self {
    let mut builder = WasiCtxBuilder::new();
    builder.inherit_stdio();

    Self {
      table: ResourceTable::new(),
      ctx: builder.build(),
      http: WasiHttpCtx::new(),
      wasi_keyvalue_ctx: WasiKeyValueCtxBuilder::new().ttl(Duration::from_secs(86400)).build(),
    }
  }
}

impl Default for State {
  fn default() -> Self {
    Self::new()
  }
}

impl IoView for State {
  fn table(&mut self) -> &mut ResourceTable {
    &mut self.table
  }
}

impl WasiView for State {
  fn ctx(&mut self) -> &mut WasiCtx {
    &mut self.ctx
  }
}

impl WasiHttpView for State {
  fn ctx(&mut self) -> &mut WasiHttpCtx {
    &mut self.http
  }

  fn send_request(
    &mut self,
    mut request: hyper::Request<HyperOutgoingBody>,
    config: OutgoingRequestConfig,
  ) -> wasmtime_wasi_http::HttpResult<HostFutureIncomingResponse>
  where
    Self: Sized,
  {
    request
      .headers_mut()
      .insert(header::USER_AGENT, HeaderValue::from_str("Octabot").unwrap());

    Ok(default_send_request(request, config))
  }
}

pub fn default_send_request(
  request: hyper::Request<HyperOutgoingBody>,
  config: OutgoingRequestConfig,
) -> HostFutureIncomingResponse {
  let handle = wasmtime_wasi::runtime::spawn(async move { Ok(default_send_request_handler(request, config).await) });
  HostFutureIncomingResponse::pending(handle)
}

pub async fn default_send_request_handler(
  request: hyper::Request<HyperOutgoingBody>,
  config: OutgoingRequestConfig,
) -> Result<IncomingResponse, ErrorCode> {
  let authority = if let Some(authority) = request.uri().authority() {
    if authority.port().is_some() {
      authority.to_string()
    } else {
      let port = if config.use_tls { 443 } else { 80 };
      format!("{}:{port}", authority)
    }
  } else {
    return Err(ErrorCode::HttpRequestUriInvalid);
  };

  let mut retries = 0;

  // Try to send the original request first
  match send_request(&authority, request, &config).await {
    Ok(response) => Ok(response),
    Err(mut error) => {
      retries += 1;

      while retries < HttpConnectionPool::MAX_RETRIES {
        sleep(Duration::from_millis(100 * 2u64.pow(retries))).await;

        match send_empty_request(&authority, &config).await {
          Ok(response) => return Ok(response),
          Err(e) => {
            error = e;
            retries += 1;
          },
        }
      }

      Err(error)
    },
  }
}

async fn send_request(
  authority: &str,
  request: hyper::Request<HyperOutgoingBody>,
  config: &OutgoingRequestConfig,
) -> Result<IncomingResponse, ErrorCode> {
  let (mut sender, worker) = HTTP_POOL
    .get_connection(authority, config.use_tls, config.connect_timeout)
    .await?;

  let resp = timeout(config.first_byte_timeout, sender.send_request(request))
    .await
    .map_err(|_| ErrorCode::ConnectionReadTimeout)?
    .map_err(hyper_request_error)?
    .map(|body| body.map_err(hyper_request_error).boxed());

  if sender.is_ready() {
    HTTP_POOL.return_connection(authority.to_string(), sender).await;
  }

  Ok(IncomingResponse {
    resp,
    worker,
    between_bytes_timeout: config.between_bytes_timeout,
  })
}

async fn send_empty_request(authority: &str, config: &OutgoingRequestConfig) -> Result<IncomingResponse, ErrorCode> {
  let (mut sender, worker) = HTTP_POOL
    .get_connection(authority, config.use_tls, config.connect_timeout)
    .await?;

  let empty_body: Empty<Bytes> = Empty::new();
  let mapped_body = empty_body.map_err(|never: Infallible| -> ErrorCode { match never {} });
  let boxed_body = BoxBody::new(mapped_body);

  let request = hyper::Request::builder()
    .method(http::Method::GET)
    .uri("/")
    .body(boxed_body)
    .map_err(|_| ErrorCode::HttpProtocolError)?;

  let resp = timeout(config.first_byte_timeout, sender.send_request(request))
    .await
    .map_err(|_| ErrorCode::ConnectionReadTimeout)?
    .map_err(hyper_request_error)?
    .map(|body| body.map_err(hyper_request_error).boxed());

  if sender.is_ready() {
    HTTP_POOL.return_connection(authority.to_string(), sender).await;
  }

  Ok(IncomingResponse {
    resp,
    worker,
    between_bytes_timeout: config.between_bytes_timeout,
  })
}

impl wasi::logging::logging::Host for State {
  async fn log(
    &mut self,
    level: wasi::logging::logging::Level,
    context: String,
    message: String,
  ) -> wasmtime::Result<()> {
    match level {
      wasi::logging::logging::Level::Trace => {
        tracing::trace!("{} {}", context, message);
      },
      wasi::logging::logging::Level::Debug => {
        tracing::debug!("{} {}", context, message);
      },
      wasi::logging::logging::Level::Info => {
        tracing::info!("{} {}", context, message);
      },
      wasi::logging::logging::Level::Warn => {
        tracing::warn!("{} {}", context, message);
      },
      wasi::logging::logging::Level::Error => {
        tracing::error!("{} {}", context, message);
      },
      wasi::logging::logging::Level::Critical => {
        tracing::error!("{} {}", context, message);
      },
    }

    Ok(())
  }
}
