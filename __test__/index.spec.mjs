import { strict as assert } from "node:assert";
import { mkdtempSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Run with `napi build` output present. napi-rs addons cannot be unit-tested
// via `cargo test` (the N-API symbols come from the Node runtime), so the
// behaviour is exercised from JS instead.
const require = createRequire(import.meta.url);
const { SearchIndex, SqliteVault } = require("../index.js");

const hits = (json) => JSON.parse(json).hits;

// --- default schema: one index-only `content` field ---
const index = new SearchIndex();
index.upsert("cn", JSON.stringify({ content: "你好世界 memory-indexer" }));
index.upsert("en", JSON.stringify({ content: "fuzzy search handles typos" }));
assert.equal(index.len(), 2);

assert.equal(hits(index.search("nihao"))[0].id, "cn", "pinyin");
assert.equal(hits(index.search("nhs"))[0].id, "cn", "pinyin initials");
assert.equal(hits(index.search("你好"))[0].id, "cn");
assert.equal(hits(index.search("search"))[0].id, "en");
// index-only: no stored payload is returned unless asked for
assert.deepEqual(hits(index.search("你好"))[0].fields, {});

const snapshot = index.checkpoint();
const restored = SearchIndex.fromCheckpoint(undefined, snapshot);
assert.equal(restored.len(), 2);
assert.equal(hits(restored.search("nihao"))[0].id, "cn");

index.delete(["en"]);
assert.equal(index.len(), 1);
assert.throws(() => index.upsert("bad", JSON.stringify({ nope: "x" })), /unknown field/);

// --- stored + positions: highlight and stored values are optional features ---
const rich = new SearchIndex(
  JSON.stringify({
    fields: [
      { name: "content", type: "text", pinyin: true, prefix: true, positions: true, stored: true },
      { name: "tag", type: "keyword", stored: true },
    ],
  }),
);
rich.upsert("a", JSON.stringify({ content: "你好世界", tag: "cn" }));

const spanned = hits(rich.search("nihao", JSON.stringify({ highlight: true })));
assert.ok(spanned[0].highlights.content.length > 0, "highlight spans");

const withFields = hits(rich.search("nihao", JSON.stringify({ storedFields: true })));
assert.equal(withFields[0].fields.content, "你好世界");
assert.equal(withFields[0].fields.tag, "cn");

// --- asset vault (shares a SQLite file) ---
const dir = mkdtempSync(join(tmpdir(), "assetvault-"));
const vault = new SqliteVault(join(dir, "test.db"), "t");

const hash = vault.putAsset("provider:character:variant", Buffer.from("PNG-BYTES"));
assert.equal(hash.length, 64, "sha3-256 hex");
assert.equal(vault.getAsset("provider:character:variant").toString(), "PNG-BYTES");
assert.equal(vault.hasAsset("provider:character:variant"), true);
assert.deepEqual(vault.listAssets(), ["provider:character:variant"]);

vault.putSnapshot("ws", 7, snapshot);
assert.equal(vault.getSnapshot("ws").length, snapshot.length);

assert.equal(vault.deleteAsset("provider:character:variant"), true);
assert.equal(vault.getAsset("provider:character:variant"), null);

console.log("assetvault tests passed");