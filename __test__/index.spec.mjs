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

// --- full-text search index ---
const index = new SearchIndex();
index.upsert("cn", JSON.stringify({ content: "你好世界 memory-indexer" }));
index.upsert("en", JSON.stringify({ content: "fuzzy search handles typos" }));
assert.equal(index.len(), 2);

assert.equal(JSON.parse(index.search("nihao")).hits[0].id, "cn");
assert.equal(JSON.parse(index.search("fuzzy")).hits[0].id, "en");

const highlighted = JSON.parse(index.search("nihao", JSON.stringify({ highlight: true })));
assert.ok(highlighted.hits[0].highlights.content.length > 0, "highlight spans");

const snapshot = index.checkpoint();
const restored = SearchIndex.fromCheckpoint(undefined, snapshot);
assert.equal(restored.len(), 2);
assert.equal(JSON.parse(restored.search("nihao")).hits[0].id, "cn");

index.delete(["en"]);
assert.equal(index.len(), 1);
assert.throws(() => index.upsert("bad", JSON.stringify({ nope: "x" })), /unknown field/);

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