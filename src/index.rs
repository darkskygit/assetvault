use std::collections::HashMap;

use memory_indexer::{
  Document, FieldId, FieldOptions, MemoryIndex, Query, Schema, SearchMode, SearchOptions, SearchResult, TextOptions,
  Value,
};
use napi::{Error, Result, bindgen_prelude::Buffer};
use napi_derive::napi;
use serde::Deserialize;

fn nerr(e: impl std::fmt::Display) -> Error {
  Error::from_reason(e.to_string())
}

#[derive(Deserialize)]
struct FieldDef {
  name: String,
  #[serde(rename = "type")]
  ty: String,
  #[serde(default)]
  pinyin: bool,
  #[serde(default)]
  prefix: bool,
  #[serde(default)]
  fuzzy: bool,
  #[serde(default)]
  positions: bool,
}

#[derive(Deserialize)]
struct SchemaDef {
  #[serde(default)]
  fields: Option<Vec<FieldDef>>,
}

fn default_fields() -> Vec<FieldDef> {
  vec![FieldDef {
    name: "content".into(),
    ty: "text".into(),
    pinyin: true,
    prefix: true,
    fuzzy: true,
    positions: true,
  }]
}

fn build_schema(schema_json: Option<&str>) -> std::result::Result<(Schema, HashMap<String, FieldId>, FieldId), String> {
  let fields = match schema_json {
    Some(raw) if !raw.trim().is_empty() => serde_json::from_str::<SchemaDef>(raw)
      .map_err(|e| e.to_string())?
      .fields
      .unwrap_or_else(default_fields),
    _ => default_fields(),
  };
  if fields.is_empty() {
    return Err("schema must declare at least one field".into());
  }

  let mut builder = Schema::builder();
  let mut ids: HashMap<String, FieldId> = HashMap::new();
  let mut first_text: Option<FieldId> = None;

  for field in &fields {
    let id = match field.ty.as_str() {
      "text" => {
        let mut text = TextOptions::multilingual();
        if field.pinyin {
          text = text.with_pinyin();
        }
        if field.prefix {
          text = text.with_prefix();
        }
        if field.fuzzy {
          text = text.with_fuzzy();
        }
        if field.positions {
          text = text.with_positions();
        }
        let id = builder.text(field.name.as_str(), text, FieldOptions::indexed_stored());
        if first_text.is_none() {
          first_text = Some(id);
        }
        id
      }
      "keyword" => builder.keyword(field.name.as_str(), FieldOptions::indexed_stored()),
      "i64" => builder.i64(field.name.as_str(), FieldOptions::indexed_stored().sortable()),
      "bool" => builder.bool(field.name.as_str(), FieldOptions::indexed_stored().sortable()),
      other => return Err(format!("unknown field type: {other}")),
    };
    ids.insert(field.name.clone(), id);
  }

  let schema = builder.build().map_err(|e| e.to_string())?;
  let first_text = first_text.ok_or_else(|| "schema must declare at least one text field".to_string())?;
  Ok((schema, ids, first_text))
}

fn json_to_values(value: &serde_json::Value) -> std::result::Result<Vec<Value>, String> {
  match value {
    serde_json::Value::String(s) => Ok(vec![Value::String(s.clone())]),
    serde_json::Value::Bool(b) => Ok(vec![Value::Bool(*b)]),
    serde_json::Value::Number(n) => match n.as_i64() {
      Some(i) => Ok(vec![Value::I64(i)]),
      None => Err(format!("only integer numbers are supported, got {n}")),
    },
    serde_json::Value::Array(items) => {
      let mut out = Vec::new();
      for item in items {
        out.extend(json_to_values(item)?);
      }
      Ok(out)
    }
    serde_json::Value::Null => Ok(Vec::new()),
    _ => Err("unsupported value type".into()),
  }
}

fn value_to_json(value: &Value) -> serde_json::Value {
  match value {
    Value::String(s) => serde_json::Value::String(s.clone()),
    Value::I64(i) => serde_json::json!(i),
    Value::Bool(b) => serde_json::Value::Bool(*b),
  }
}

fn parse_mode(mode: Option<&str>) -> SearchMode {
  match mode.map(str::to_ascii_lowercase).as_deref() {
    Some("exact") => SearchMode::Exact,
    Some("pinyin") => SearchMode::Pinyin,
    Some("fuzzy") => SearchMode::Fuzzy,
    _ => SearchMode::Auto,
  }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SearchOpts {
  field: Option<String>,
  mode: Option<String>,
  limit: Option<usize>,
  offset: Option<usize>,
  highlight: Option<bool>,
}

fn serialize_result(schema: &Schema, result: &SearchResult) -> serde_json::Value {
  let field_name = |id: FieldId| -> String { schema.field(id).map(|f| f.name.clone()).unwrap_or_default() };

  let hits: Vec<serde_json::Value> = result
    .hits
    .iter()
    .map(|hit| {
      let mut fields = serde_json::Map::new();
      for (id, values) in &hit.fields {
        let mut json_values: Vec<serde_json::Value> = values.iter().map(value_to_json).collect();
        let value = if json_values.len() == 1 {
          json_values.pop().unwrap()
        } else {
          serde_json::Value::Array(json_values)
        };
        fields.insert(field_name(*id), value);
      }
      let mut highlights = serde_json::Map::new();
      for highlight in &hit.highlights {
        let spans: Vec<serde_json::Value> = highlight
          .spans
          .iter()
          .map(|(start, end)| serde_json::json!([start, end]))
          .collect();
        highlights.insert(field_name(highlight.field), serde_json::Value::Array(spans));
      }
      serde_json::json!({
        "id": hit.id,
        "score": hit.score,
        "fields": fields,
        "highlights": highlights,
      })
    })
    .collect();

  serde_json::json!({ "total": result.total, "hits": hits })
}

/// In-memory multilingual full-text index.
#[napi]
pub struct SearchIndex {
  schema: Schema,
  fields: HashMap<String, FieldId>,
  stored_fields: Vec<FieldId>,
  default_text_field: FieldId,
  index: MemoryIndex,
}

#[napi]
impl SearchIndex {
  /// Create an empty index. `schemaJson` is optional; when omitted a single
  /// `content` text field (pinyin + prefix + fuzzy + positions) is used.
  #[napi(constructor)]
  pub fn new(schema_json: Option<String>) -> Result<Self> {
    let (schema, fields, default_text_field) = build_schema(schema_json.as_deref()).map_err(nerr)?;
    let stored_fields: Vec<FieldId> = fields.values().copied().collect();
    let index = MemoryIndex::new(schema.clone());
    Ok(Self {
      schema,
      fields,
      stored_fields,
      default_text_field,
      index,
    })
  }

  /// Create an index from a checkpoint produced by `checkpoint()`.
  #[napi(factory)]
  pub fn from_checkpoint(schema_json: Option<String>, bytes: Buffer) -> Result<Self> {
    let (schema, fields, default_text_field) = build_schema(schema_json.as_deref()).map_err(nerr)?;
    let stored_fields: Vec<FieldId> = fields.values().copied().collect();
    let index = MemoryIndex::from_checkpoint(schema.clone(), &bytes).map_err(nerr)?;
    Ok(Self {
      schema,
      fields,
      stored_fields,
      default_text_field,
      index,
    })
  }

  /// Insert or replace one document. `fieldsJson` maps field name -> value
  /// (string | integer | boolean | array of those).
  #[napi]
  pub fn upsert(&mut self, id: String, fields_json: String) -> Result<()> {
    let object: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&fields_json).map_err(nerr)?;
    let mut document = Document::new(id);
    for (name, value) in object {
      let field = self
        .fields
        .get(&name)
        .copied()
        .ok_or_else(|| Error::from_reason(format!("unknown field: {name}")))?;
      let values = json_to_values(&value).map_err(nerr)?;
      document.add_values(field, values);
    }
    self.index.upsert(document).map_err(nerr)?;
    Ok(())
  }

  /// Delete documents by id; returns the number actually removed.
  #[napi]
  pub fn delete(&mut self, ids: Vec<String>) -> u32 {
    let mut removed = 0u32;
    for id in ids {
      if self.index.delete(&id) {
        removed += 1;
      }
    }
    removed
  }

  /// Full-text search. `optionsJson` = { field?, mode?, limit?, offset?,
  /// highlight? }. Returns JSON `{ total, hits: [{ id, score, fields,
  /// highlights }] }`.
  #[napi]
  pub fn search(&self, query: String, options_json: Option<String>) -> Result<String> {
    let options: SearchOpts = match options_json.as_deref() {
      Some(raw) if !raw.trim().is_empty() => serde_json::from_str(raw).map_err(nerr)?,
      _ => SearchOpts::default(),
    };
    let field = match options.field.as_deref() {
      Some(name) => self
        .fields
        .get(name)
        .copied()
        .ok_or_else(|| Error::from_reason(format!("unknown field: {name}")))?,
      None => self.default_text_field,
    };

    let mut search = SearchOptions::new(options.limit.unwrap_or(10));
    search.offset = options.offset.unwrap_or(0);
    search.stored_fields = self.stored_fields.clone();
    if options.highlight.unwrap_or(false) {
      search.highlight_fields = vec![field];
    }

    let result = self
      .index
      .search(&Query::text(field, query, parse_mode(options.mode.as_deref())), search)
      .map_err(nerr)?;
    serde_json::to_string(&serialize_result(&self.schema, &result)).map_err(nerr)
  }

  /// Number of live documents.
  #[napi]
  pub fn len(&self) -> u32 {
    self.index.len() as u32
  }

  /// Compressed snapshot bytes (store these in a SQLite BLOB).
  #[napi]
  pub fn checkpoint(&self) -> Result<Buffer> {
    let checkpoint = self.index.checkpoint().map_err(nerr)?;
    Ok(Buffer::from(checkpoint.bytes))
  }

  /// Change sequence of the latest checkpoint.
  #[napi]
  pub fn sequence(&self) -> i64 {
    self.index.change_sequence() as i64
  }

  #[napi]
  pub fn has_unpersisted_changes(&self) -> bool {
    self.index.has_unpersisted_changes()
  }

  /// Acknowledge that `sequence` was persisted.
  #[napi]
  pub fn mark_persisted(&mut self, sequence: i64) -> Result<()> {
    self.index.mark_checkpoint_persisted(sequence as u64).map_err(nerr)
  }

  /// Load a checkpoint, replacing the current contents.
  #[napi]
  pub fn load(&mut self, bytes: Buffer) -> Result<()> {
    self.index = MemoryIndex::from_checkpoint(self.schema.clone(), &bytes).map_err(nerr)?;
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn upsert_search_and_checkpoint_roundtrip() {
    let mut index = SearchIndex::new(None).unwrap();
    index
      .upsert("a".into(), "{\"content\":\"你好世界 memory-indexer\"}".into())
      .unwrap();

    let result: serde_json::Value = serde_json::from_str(&index.search("nihao".into(), None).unwrap()).unwrap();
    assert_eq!(result["total"], 1);
    assert_eq!(result["hits"][0]["id"], "a");

    let snapshot = index.checkpoint().unwrap();
    let restored = SearchIndex::from_checkpoint(None, snapshot).unwrap();
    assert_eq!(restored.len(), 1);
  }

  #[test]
  fn unknown_field_is_rejected() {
    let mut index = SearchIndex::new(None).unwrap();
    assert!(index.upsert("a".into(), "{\"nope\":\"x\"}".into()).is_err());
  }
}
