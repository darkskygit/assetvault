use std::time::{Duration, SystemTime, UNIX_EPOCH};

use assetpack_core::{
  Codec, FileHint, FileReadLimits, FileReader, Hash32, ObjectKind, ObjectRecord, Pipeline, PipelineConfig,
  RusqliteStore, TransformDecoderRegistry, build_recipe,
};
use napi::{Error, Result, bindgen_prelude::Buffer};
use napi_derive::napi;
use rusqlite::{Connection, OptionalExtension, params};

fn nerr(e: impl std::fmt::Display) -> Error {
  Error::from_reason(e.to_string())
}

fn now_millis() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis() as i64)
    .unwrap_or(0)
}

fn sanitize_namespace(raw: &str) -> String {
  let cleaned: String = raw
    .chars()
    .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
    .collect();
  if cleaned.is_empty() {
    "default".to_string()
  } else {
    cleaned
  }
}

fn aux_schema(ns: &str) -> String {
  format!(
    "CREATE TABLE IF NOT EXISTS {ns}_asset_keys(
       key TEXT PRIMARY KEY,
       recipe_hash TEXT NOT NULL,
       file_hash TEXT NOT NULL,
       size INTEGER NOT NULL,
       updated_at INTEGER NOT NULL);
     CREATE INDEX IF NOT EXISTS idx_{ns}_asset_keys_recipe ON {ns}_asset_keys(recipe_hash);
     CREATE TABLE IF NOT EXISTS {ns}_snapshot(
       workspace TEXT PRIMARY KEY,
       sequence INTEGER NOT NULL,
       bytes BLOB NOT NULL,
       updated_at INTEGER NOT NULL);"
  )
}

/// SQLite-backed content-addressed vault. Shares one database file with the
/// host application; all tables are prefixed with `{namespace}_`.
#[napi]
pub struct SqliteVault {
  conn: Connection,
  namespace: String,
}

#[napi]
impl SqliteVault {
  #[napi(constructor)]
  pub fn new(db_path: String, namespace: Option<String>) -> Result<Self> {
    let conn = Connection::open(&db_path).map_err(nerr)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;").map_err(nerr)?;
    conn.busy_timeout(Duration::from_millis(5000)).map_err(nerr)?;
    let namespace = sanitize_namespace(namespace.as_deref().unwrap_or("default"));
    RusqliteStore::from_connection_with_namespace(&conn, &namespace).map_err(nerr)?;
    conn.execute_batch(&aux_schema(&namespace)).map_err(nerr)?;
    Ok(Self { conn, namespace })
  }

  fn store(&self) -> Result<RusqliteStore<'_>> {
    RusqliteStore::from_connection_with_namespace(&self.conn, &self.namespace).map_err(nerr)
  }

  /// Store bytes under a logical key. Returns the SHA3-256 content hash (hex).
  #[napi]
  pub fn put_asset(&mut self, key: String, data: Buffer) -> Result<String> {
    let bytes = data.to_vec();
    let size = bytes.len();
    let file_hash = Hash32::sha3_256(&bytes);
    let hint = FileHint {
      size: size as u64,
      extension: None,
      head: None,
    };
    let plan = Pipeline::new(PipelineConfig::default())
      .run(bytes, &hint, file_hash, None)
      .map_err(nerr)?;

    let chunk_list: Vec<(Hash32, u32)> = plan.chunks.iter().map(|c| (c.hash, c.raw_len)).collect();
    let recipe = build_recipe(
      plan.original_size,
      &chunk_list,
      file_hash,
      plan.transform_id,
      plan.transform_version,
    );
    let recipe_hash = Hash32::sha3_256(&recipe);
    let objects: Vec<ObjectRecord> = plan
      .chunks
      .into_iter()
      .map(|chunk| ObjectRecord {
        hash: chunk.hash,
        kind: ObjectKind::Chunk,
        decoded_len: u64::from(chunk.raw_len),
        codec: chunk.codec,
        stored_bytes: chunk.payload.unwrap_or_default(),
      })
      .collect();

    let store = self.store()?;
    store.put_objects_batch(&objects).map_err(nerr)?;
    store.put_recipe(recipe_hash, &recipe, Codec::Raw).map_err(nerr)?;

    self
      .conn
      .execute(
        &format!(
          "INSERT INTO {0}_asset_keys(key, recipe_hash, file_hash, size, updated_at)
           VALUES(?1, ?2, ?3, ?4, ?5)
           ON CONFLICT(key) DO UPDATE SET
             recipe_hash=?2, file_hash=?3, size=?4, updated_at=?5",
          self.namespace
        ),
        params![key, recipe_hash.to_hex(), file_hash.to_hex(), size as i64, now_millis()],
      )
      .map_err(nerr)?;

    Ok(file_hash.to_hex())
  }

  #[napi]
  pub fn get_asset(&self, key: String) -> Result<Option<Buffer>> {
    let recipe_hex: Option<String> = self
      .conn
      .query_row(
        &format!("SELECT recipe_hash FROM {0}_asset_keys WHERE key=?1", self.namespace),
        params![key],
        |row| row.get(0),
      )
      .optional()
      .map_err(nerr)?;
    let Some(recipe_hex) = recipe_hex else {
      return Ok(None);
    };
    let recipe_hash = Hash32::from_hex(&recipe_hex).map_err(nerr)?;
    let store = self.store()?;
    let decoders = TransformDecoderRegistry::default();
    let reader = FileReader::new(&store, &decoders, FileReadLimits::default());
    let bytes = reader.read_file(recipe_hash).map_err(nerr)?;
    Ok(Some(Buffer::from(bytes)))
  }

  #[napi]
  pub fn has_asset(&self, key: String) -> Result<bool> {
    let count: i64 = self
      .conn
      .query_row(
        &format!("SELECT COUNT(*) FROM {0}_asset_keys WHERE key=?1", self.namespace),
        params![key],
        |row| row.get(0),
      )
      .map_err(nerr)?;
    Ok(count > 0)
  }

  #[napi]
  pub fn delete_asset(&mut self, key: String) -> Result<bool> {
    let changed = self
      .conn
      .execute(
        &format!("DELETE FROM {0}_asset_keys WHERE key=?1", self.namespace),
        params![key],
      )
      .map_err(nerr)?;
    Ok(changed > 0)
  }

  /// All stored keys (useful for GC / listing).
  #[napi]
  pub fn list_assets(&self) -> Result<Vec<String>> {
    let mut statement = self
      .conn
      .prepare(&format!("SELECT key FROM {0}_asset_keys ORDER BY key", self.namespace))
      .map_err(nerr)?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0)).map_err(nerr)?;
    let mut keys = Vec::new();
    for row in rows {
      keys.push(row.map_err(nerr)?);
    }
    Ok(keys)
  }

  /// Persist an index checkpoint for a workspace.
  #[napi]
  pub fn put_snapshot(&mut self, workspace: String, sequence: i64, bytes: Buffer) -> Result<()> {
    self
      .conn
      .execute(
        &format!(
          "INSERT INTO {0}_snapshot(workspace, sequence, bytes, updated_at)
           VALUES(?1, ?2, ?3, ?4)
           ON CONFLICT(workspace) DO UPDATE SET sequence=?2, bytes=?3, updated_at=?4",
          self.namespace
        ),
        params![workspace, sequence, bytes.to_vec(), now_millis()],
      )
      .map_err(nerr)?;
    Ok(())
  }

  #[napi]
  pub fn get_snapshot(&self, workspace: String) -> Result<Option<Buffer>> {
    let bytes: Option<Vec<u8>> = self
      .conn
      .query_row(
        &format!("SELECT bytes FROM {0}_snapshot WHERE workspace=?1", self.namespace),
        params![workspace],
        |row| row.get(0),
      )
      .optional()
      .map_err(nerr)?;
    Ok(bytes.map(Buffer::from))
  }

  #[napi]
  pub fn delete_snapshot(&mut self, workspace: String) -> Result<bool> {
    let changed = self
      .conn
      .execute(
        &format!("DELETE FROM {0}_snapshot WHERE workspace=?1", self.namespace),
        params![workspace],
      )
      .map_err(nerr)?;
    Ok(changed > 0)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn asset_and_snapshot_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.db");
    let mut vault = SqliteVault::new(db.to_string_lossy().into_owned(), Some("t".into())).unwrap();

    let hash = vault
      .put_asset("k".into(), Buffer::from(b"hello world".to_vec()))
      .unwrap();
    assert_eq!(hash.len(), 64, "sha3-256 hex length");

    let got = vault.get_asset("k".into()).unwrap().unwrap();
    assert_eq!(got.to_vec(), b"hello world");
    assert!(vault.has_asset("k".into()).unwrap());

    vault
      .put_snapshot("ws".into(), 7, Buffer::from(vec![1u8, 2, 3]))
      .unwrap();
    assert_eq!(
      vault.get_snapshot("ws".into()).unwrap().unwrap().to_vec(),
      vec![1u8, 2, 3]
    );

    assert!(vault.delete_asset("k".into()).unwrap());
    assert!(vault.get_asset("k".into()).unwrap().is_none());
  }
}
