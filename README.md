# assetvault

In-memory multilingual full-text index + SQLite content-addressed asset vault for Node.js

## Install

```sh
npm install assetvault
# or pnpm add assetvault
```

Supported platforms: `darwin-arm64`, `win32-x64`, `win32-arm64`.

## Usage

```js
const { SearchIndex, SqliteVault } = require('assetvault')

// --- full-text index ---
// Optional schema. When omitted, a single `content` text field is used.
const index = new SearchIndex(
  JSON.stringify({
    fields: [
      { name: 'title', type: 'text', fuzzy: true },
      { name: 'content', type: 'text', pinyin: true, prefix: true, fuzzy: true, positions: true },
      { name: 'tags', type: 'keyword' },
      { name: 'rank', type: 'i64' }
    ]
  })
)

index.upsert('doc-1', JSON.stringify({ title: '你好', content: '你好世界 memory-indexer', rank: 1 }))

const result = JSON.parse(
  index.search('nihao', JSON.stringify({ limit: 10, highlight: true }))
)
// result.hits[0].id, .score, .fields, .highlights

// snapshot (store the Buffer anywhere, e.g. a BLOB column)
const snapshot = index.checkpoint()
index.markPersisted(index.sequence())

// --- asset vault (shares your SQLite file) ---
const vault = new SqliteVault('app.db', 'iv_main')
const contentHash = vault.putAsset('provider:character:variant', Buffer.from(pngBytes))
const same = vault.getAsset('provider:character:variant') // Buffer | null

// index snapshot persisted next to assets
vault.putSnapshot('default', index.sequence(), snapshot)
const restored = SearchIndex.fromCheckpoint(undefined, vault.getSnapshot('default'))
```

### Schema fields

| `type` | notes |
| --- | --- |
| `text` | `pinyin`, `prefix`, `fuzzy`, `positions` flags (all default off; set as needed) |
| `keyword` | exact term matching |
| `i64` | integer, sortable |
| `bool` | boolean, sortable |

Values are `string | integer | boolean | array of those`. Floats are rejected.

### Search options

`{ field?, mode?: 'auto'|'exact'|'fuzzy'|'pinyin', limit?, offset?, highlight? }`
`mode` defaults to `auto`.

## API

```ts
class SearchIndex {
  constructor(schemaJson?: string)
  static fromCheckpoint(schemaJson: string | undefined, bytes: Buffer): SearchIndex
  upsert(id: string, fieldsJson: string): void
  delete(ids: string[]): number
  search(query: string, optionsJson?: string): string   // JSON result
  len(): number
  checkpoint(): Buffer
  load(bytes: Buffer): void
  sequence(): number
  hasUnpersistedChanges(): boolean
  markPersisted(sequence: number): void
}

class SqliteVault {
  constructor(dbPath: string, namespace?: string)
  putAsset(key: string, data: Buffer): string           // returns content hash (hex)
  getAsset(key: string): Buffer | null
  hasAsset(key: string): boolean
  deleteAsset(key: string): boolean
  listAssets(): string[]
  putSnapshot(workspace: string, sequence: number, bytes: Buffer): void
  getSnapshot(workspace: string): Buffer | null
  deleteSnapshot(workspace: string): boolean
}
```

The vault sets `journal_mode=WAL` and `busy_timeout=5000` on its connection, so it
can safely coexist with another connection to the same file.

## Building from source

```sh
rustup target add aarch64-apple-darwin x86_64-pc-windows-msvc aarch64-pc-windows-msvc
npm install
npm run build          # cross-platform: --target <triple> --cross-compile
```

`--cross-compile` builds Windows MSVC targets from macOS via `cargo-xwin`.

## License

AGPL-3.0-or-later. See [LICENSE](./LICENSE).
