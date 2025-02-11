mod generated {
  wasmtime::component::bindgen!({
      path: "wit",
      world: "wasi:keyvalue/imports",
      trappable_imports: true,
      with: {
          "wasi:keyvalue/store/bucket": crate::keyvalue::Bucket,
      },
      trappable_error_type: {
          "wasi:keyvalue/store/error" => crate::keyvalue::Error,
      },
  });
}

use self::generated::wasi::keyvalue;

use anyhow::Result;
use parking_lot::Mutex;
use std::time::{Duration, Instant};
use std::{collections::HashMap, sync::Arc};
use wasmtime::component::{Resource, ResourceTable, ResourceTableError};

struct CacheEntry {
  value: Vec<u8>,
  expires_at: Instant,
}

#[doc(hidden)]
pub enum Error {
  NoSuchStore,
  AccessDenied,
  Other(String),
}

impl From<ResourceTableError> for Error {
  fn from(err: ResourceTableError) -> Self {
    Self::Other(err.to_string())
  }
}

#[doc(hidden)]
pub struct Bucket {
  shared_data: Arc<Mutex<HashMap<String, CacheEntry>>>,
}

/// Builder-style structure used to create a [`WasiKeyValueCtx`].
pub struct WasiKeyValueCtxBuilder {
  in_memory_data: HashMap<String, Vec<u8>>,
  ttl: Duration,
}

impl Default for WasiKeyValueCtxBuilder {
  fn default() -> Self {
    Self {
      in_memory_data: HashMap::new(),
      ttl: Duration::from_secs(86400), // Default 1 day TTL
    }
  }
}

impl WasiKeyValueCtxBuilder {
  /// Creates a builder for a new context with default parameters set.
  pub fn new() -> Self {
    Default::default()
  }

  pub fn ttl(mut self, duration: Duration) -> Self {
    self.ttl = duration;
    self
  }

  /// Preset data for the In-Memory provider.
  pub fn in_memory_data<I, K, V>(mut self, data: I) -> Self
  where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<Vec<u8>>,
  {
    self.in_memory_data = data.into_iter().map(|(k, v)| (k.into(), v.into())).collect();
    self
  }

  /// Uses the configured context so far to construct the final [`WasiKeyValueCtx`].
  pub fn build(self) -> WasiKeyValueCtx {
    let now = Instant::now();
    let cache_data: HashMap<String, CacheEntry> = self
      .in_memory_data
      .into_iter()
      .map(|(k, v)| {
        (
          k,
          CacheEntry {
            value: v,
            expires_at: now + self.ttl,
          },
        )
      })
      .collect();

    WasiKeyValueCtx {
      in_memory_data: Arc::new(Mutex::new(cache_data)),
    }
  }
}

fn cleanup_expired_entries(data: &mut HashMap<String, CacheEntry>) {
  let now = Instant::now();
  data.retain(|_, entry| entry.expires_at > now);
}

/// Capture the state necessary for use in the `wasi-keyvalue` API implementation.
pub struct WasiKeyValueCtx {
  in_memory_data: Arc<Mutex<HashMap<String, CacheEntry>>>,
}

impl WasiKeyValueCtx {
  /// Convenience function for calling [`WasiKeyValueCtxBuilder::new`].
  pub fn builder() -> WasiKeyValueCtxBuilder {
    WasiKeyValueCtxBuilder::new()
  }
}

/// A wrapper capturing the needed internal `wasi-keyvalue` state.
pub struct WasiKeyValue<'a> {
  ctx: &'a WasiKeyValueCtx,
  table: &'a mut ResourceTable,
}

impl<'a> WasiKeyValue<'a> {
  /// Create a new view into the `wasi-keyvalue` state.
  pub fn new(ctx: &'a WasiKeyValueCtx, table: &'a mut ResourceTable) -> Self {
    Self { ctx, table }
  }
}

impl keyvalue::store::Host for WasiKeyValue<'_> {
  fn open(&mut self, identifier: String) -> Result<Resource<Bucket>, Error> {
    match identifier.as_str() {
      "" => Ok(self.table.push(Bucket {
        shared_data: self.ctx.in_memory_data.clone(),
      })?),
      _ => Err(Error::NoSuchStore),
    }
  }

  fn convert_error(&mut self, err: Error) -> Result<keyvalue::store::Error> {
    match err {
      Error::NoSuchStore => Ok(keyvalue::store::Error::NoSuchStore),
      Error::AccessDenied => Ok(keyvalue::store::Error::AccessDenied),
      Error::Other(e) => Ok(keyvalue::store::Error::Other(e)),
    }
  }
}

impl keyvalue::store::HostBucket for WasiKeyValue<'_> {
  fn get(&mut self, bucket: Resource<Bucket>, key: String) -> Result<Option<Vec<u8>>, Error> {
    let bucket = self.table.get(&bucket)?;
    let mut data = bucket.shared_data.lock();

    // Clean up expired entries
    cleanup_expired_entries(&mut data);

    // Return cloned value if it exists and hasn't expired
    Ok(data.get(&key).map(|entry| entry.value.clone()))
  }

  fn set(&mut self, bucket: Resource<Bucket>, key: String, value: Vec<u8>) -> Result<(), Error> {
    let bucket = self.table.get(&bucket)?;
    let mut data = bucket.shared_data.lock();

    // Clean up expired entries
    cleanup_expired_entries(&mut data);

    // Insert new entry with current time + TTL
    data.insert(
      key,
      CacheEntry {
        value,
        expires_at: Instant::now() + Duration::from_secs(3600), // You might want to make this configurable
      },
    );
    Ok(())
  }

  fn delete(&mut self, bucket: Resource<Bucket>, key: String) -> Result<(), Error> {
    let bucket = self.table.get(&bucket)?;
    let mut data = bucket.shared_data.lock();

    // Clean up expired entries
    cleanup_expired_entries(&mut data);

    data.remove(&key);
    Ok(())
  }

  fn exists(&mut self, bucket: Resource<Bucket>, key: String) -> Result<bool, Error> {
    let bucket = self.table.get(&bucket)?;
    let mut data = bucket.shared_data.lock();

    // Clean up expired entries
    cleanup_expired_entries(&mut data);

    Ok(data.contains_key(&key))
  }

  fn list_keys(
    &mut self,
    bucket: Resource<Bucket>,
    cursor: Option<u64>,
  ) -> Result<keyvalue::store::KeyResponse, Error> {
    let bucket = self.table.get(&bucket)?;
    let mut data = bucket.shared_data.lock();

    // Clean up expired entries
    cleanup_expired_entries(&mut data);

    let keys: Vec<String> = data.keys().cloned().collect();
    let cursor = cursor.unwrap_or(0) as usize;
    let keys_slice = &keys[cursor..];
    Ok(keyvalue::store::KeyResponse {
      keys: keys_slice.to_vec(),
      cursor: None,
    })
  }

  fn drop(&mut self, bucket: Resource<Bucket>) -> Result<()> {
    self.table.delete(bucket)?;
    Ok(())
  }
}

/// Add all the `wasi-keyvalue` world's interfaces to a [`wasmtime::component::Linker`].
pub fn add_to_linker<T: Send>(
  l: &mut wasmtime::component::Linker<T>,
  f: impl Fn(&mut T) -> WasiKeyValue<'_> + Send + Sync + Copy + 'static,
) -> Result<()> {
  keyvalue::store::add_to_linker_get_host(l, f)?;
  Ok(())
}
