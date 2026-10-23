# What auth-framework's `AuthStorage` needs from persistant-kv

Status: **design note, 2026-10-23, nothing built.** Written against PersistAnt `ccad2b4` (0.9.17) and
auth-framework's working tree (`src/storage/core.rs`, `sqlite.rs`, `redis.rs`). Origin: PM [TASK] 2026-10-23,
from the auth-framework lane's finding "persistant for `SqliteStorage`: not yet" (its HANDOFF item 4).

## 1. The finding, corrected

auth-framework's lane listed `compare_and_swap`, `take`, `batch_atomic`, `ttl_remaining` and `count` as missing
from persistant-kv. At `ccad2b4` that is **true for two of the five and false for three**, and true only for
some backends. Read from the checked-in matrices (`matrix/*.tsv`, the `enabled` column; the CI regenerates and
compares them, so they are what the code certifies):

| `kv.*` leaf | memory (`MemoryKv`) | sqlite, sqlite-full | postgres | mysql, mariadb | opendal fs / memory (`KvOnBlob`) |
|---|---|---|---|---|---|
| `get` `put` `delete` `ttl` `prefix_list` `ordered_scan` `put_if_absent` | yes | yes | yes | yes | yes (`put_if_absent` only without a ttl) |
| `compare_and_swap`, `compare_and_delete` | **yes** | **yes** | **yes** | **yes** | no |
| `batch_atomic.*` (`batch` with `Require`) | **yes** | **yes** | **yes** | **yes** | no |
| `ordered_scan_values` (values from one snapshot) | yes | yes | yes | yes | no |
| `ttl_remaining`, `purge_expired` | yes | yes | yes | yes | `ttl_remaining` yes, `purge_expired` no |
| `take` (get-and-delete, exactly one winner) | **no: in the vocabulary, no trait method** | no | no | no | no |
| `count` (live keys under a prefix) | **no: in the vocabulary, no trait method** | no | no | no | no |

(Memory's row is its `impl KvBackend`, which has every method; the matrices carry the SQL and opendal rows.
There is no Redis backend in PersistAnt.) The vocabulary reserves both missing leaves already
(`persistant-core/src/vocabulary.rs`: `kv.take`, `kv.count`).

## 2. What `AuthStorage` actually asks for

From `src/storage/core.rs` and the SQL that `sqlite.rs` runs for it:

| Need | Where it comes from | What it asks of the store |
|---|---|---|
| token by id, by access token, list by user | `get_token`, `get_token_by_access_token`, `list_user_tokens`; `idx_auth_tokens_{user_id,access_token}` | a **secondary-key lookup** that stays consistent with the primary record |
| session by id, list by user | `get_session`, `list_user_sessions`; `idx_sessions_user_id` | the same, one index |
| upsert, delete | `ON CONFLICT .. DO UPDATE`, `DELETE .. WHERE id = ?` | `put`, `delete` |
| update a token whose access token changed | `update_token` | replace the primary record **and** move its index entry, together |
| count active sessions | `SELECT COUNT(*) .. WHERE expires_at > ?` | `count` of live keys under a prefix |
| kv with ttl, delete, prefix list | `store_kv` `get_kv` `delete_kv` `list_kv_keys` | `put` with ttl, `get`, `delete`, `prefix_list` (exist) |
| cleanup of expired rows | `cleanup_expired` | `purge_expired` (exists) |
| single-use consume (a PAR request URI, an authorization code) | design note `DESIGN-KINDS.md` section 1: today `get` then `delete` (`par.rs:268-300`), racy | **`take`**: get-and-delete, one winner |
| swap | the lane's note mentions `supports_atomic_swap`; no such identifier exists in this tree (grep of `*.rs`: none) | `compare_and_swap` (exists) |

Two things the SQLite implementation does that persistant-kv would *improve*: it truncates a ttl to whole
seconds (`DESIGN-KINDS.md` section 1), and persistant-kv stores milliseconds; and `list_kv_keys` silently
returns empty on three of the framework's backends, where persistant-kv's `prefix_list` is a certified leaf.

## 3. Per need, per backend: native, emulable, impossible

"Native" = one engine operation, certified by a matrix leaf. "Emulable" = a correct construction over certified
leaves, no new engine code. "Impossible" = the backend lacks the primitive the construction needs; it is
refused (`KvError::Unsupported`, the matrix says `no`), never approximated.

| Need | memory | sqlite / postgres / mysql / mariadb | opendal fs / memory | Redis (not a PersistAnt backend) |
|---|---|---|---|---|
| swap | native | native | **impossible** (no compare-and-delete in a blob store; `kv_on_blob.rs` says so; S3-class conditional writes (`if-match`) might change that per service: unverified) | native (`WATCH`/`EVAL`) |
| take | emulable over `compare_and_delete` (section 4.1); native later | emulable; native `DELETE .. RETURNING` on postgres, sqlite 3.35+, mariadb 10.5+ (MySQL has none) | impossible (needs `compare_and_delete`) | native `GETDEL` |
| atomic batch | native | native | **impossible** (no multi-key transaction) | native (`MULTI`/`EVAL`) |
| ttl_remaining | native | native | native | native (`PTTL`) |
| count under a prefix | native, one snapshot | native (`SELECT count(*)` over the key range), one snapshot | emulable by paging `prefix_list`, **not** a snapshot (name it so) | emulable (`SCAN`); not a snapshot |
| secondary-key lookup | emulable as a layer over `batch` + `ordered_scan_values` (section 4.3) | same | **impossible**: with no `batch`, the primary record and its index entries can diverge | emulable via the same layer once Redis has `batch` |

The effect is a clean line: **memory and the four SQL engines can carry `AuthStorage` as it is;
opendal-backed stores cannot, and should say so** (the registry's `open_named` already refuses a backend whose
declared leaves do not cover what the caller needs). A Redis backend would be a separate piece of work,
would be native for every row, and is not proposed here.

## 4. The minimal extension

Nothing here changes an existing method. Three additions, in this order, each its own PR with a matrix row.

### 4.1 `KvBackend::take` (leaf `kv.take`)

```rust
/// Remove `key` and return the live value it held, to exactly one of any number of concurrent callers;
/// `None` if absent or expired (`kv.take`).
fn take(&self, key: &str) -> impl Future<Output = Result<Option<Vec<u8>>, KvError>> + Send { .. }
```

**No new primitive.** The default body is `get`, then `compare_and_delete(key, &value)`: `true` returns the value,
`false` means the value changed between the two calls, so read again (bounded, say 16 rounds, then
`KvError::Backend("take: contended")`). Exactly-one-winner is inherited from `compare_and_delete`'s own
certified guarantee. A backend without `compare_and_delete` therefore has no `take` (the default returns
`Unsupported` when `compare_and_delete` does), so the matrix stays truthful. Backends may override with a native
form later (`DELETE .. RETURNING`), proven by the same conformance test. The test: N tasks `take` one key;
one gets `Some`, the others `None`; and a key rewritten while being taken is never returned twice.

### 4.2 `KvBackend::count` (leaf `kv.count`)

```rust
/// The number of live keys beginning with `prefix`, from one consistent snapshot (`kv.count`).
fn count(&self, prefix: &str) -> impl Future<Output = Result<u64, KvError>> + Send { .. } // default: Unsupported
```

Native on memory and the SQL engines only, where one statement counts a key range under the table's own
snapshot. For backends that cannot, a free function `count_prefix_paged(&kv, prefix)` pages `prefix_list`
and is documented as *not* a snapshot; it is not the trait method, so the matrix does not claim `kv.count`
for it. `count_active_sessions` is an analytics read, so the paged form would be acceptable to it where
needed, but the framework should know which one it got.

### 4.3 Secondary keys as a layer, not a method: `persistant_kv::index`

A secondary index is not something a backend offers; it is a *discipline* over `batch`. Proposed: a small
wrapper type, eligible by derivation exactly like `KvOnBlob` is (it needs `kv.batch_atomic` and
`kv.ordered_scan_values`, so it exists for memory and the SQL engines and is refused elsewhere):

```rust
pub struct Indexed<K> { kv: K }
impl<K: KvBackend> Indexed<K> {
    /// Store `value` at `primary`, and an empty entry at each of `indexes`, in one batch.
    pub async fn put(&self, primary: &str, value: Vec<u8>, indexes: &[&str], ttl: Option<Duration>) -> Result<(), KvError>;
    /// Replace `primary`'s value and index set if it still holds `expected` (a `Require`); `Ok(false)` if not.
    pub async fn replace(&self, primary: &str, expected: &[u8], value: Vec<u8>, old: &[&str], new: &[&str], ttl: Option<Duration>) -> Result<bool, KvError>;
    /// Remove `primary` and its index entries together.
    pub async fn remove(&self, primary: &str, indexes: &[&str]) -> Result<(), KvError>;
    /// The primary keys named by index entries under `index_prefix`, then their values, from one snapshot.
    pub async fn lookup(&self, index_prefix: &str) -> Result<Vec<(String, Vec<u8>)>, KvError>;
}
```

Key layout the framework would use (strings; the store is bytes):

| Key | Value | For |
|---|---|---|
| `tok:{token_id}` | the token record | `get_token` |
| `tokat:{access_token}` | the `token_id` | `get_token_by_access_token`: one `get`, one `get` |
| `tokuser:{user_id}:{token_id}` | empty | `list_user_tokens`: scan the prefix, then fetch each |
| `ses:{session_id}` | the session record | `get_session` |
| `sesuser:{user_id}:{session_id}` | empty | `list_user_sessions` |

Every write goes through one `batch`, so a crash or a concurrent `update_token` never leaves a record without
its index entries or the reverse. All of an entry's keys carry the **same ttl**, taken from the record's
`expires_at`, so they expire together and `count` over `ses:` is the number of active sessions without a
`WHERE expires_at > ?`. The cost against SQLite is honest: an access-token lookup is two reads where an
indexed `SELECT` is one, and each token is three or four keys instead of one row. (If a measurement says the
second read matters, the index value can carry the record itself, trading write size for one read; decide on
numbers, not now.)

### 4.4 What does not change

* The `AuthStorage` trait, the token and session types, JSON encoding and RBAC tables stay in
  auth-framework. RBAC (`AbacRole`, `UserRole`) is relational data, not key-value, and belongs on
  `RelationalBackend` (or stays as it is).
* No new backend. No change to the registry beyond declaring that an `AuthStorage` open needs
  `compare_and_swap`, `take`, `batch_atomic`, `ordered_scan_values`, `ttl_remaining` and `count`.

## 5. What this does not establish

* **Per-tenant create on first use and file permissions** (Commy's `<tenant>.sqlite`): the auth lane left them
  "unverified", and so does this note. `KvOnRelational::open` runs `CREATE TABLE IF NOT EXISTS`, and
  `SqliteBackend::open(path)` takes a path, but whether it creates the file, with which mode, and whether a
  per-tenant open cost is acceptable has not been measured. A small test (open a path that does not exist as
  a non-owner; stat the mode) settles it and should come before the first PR.
* **Throughput against the SQL the framework runs today.** Not measured. The index layer adds keys per
  record; the existing `speed` harness (`kv.speed`) is the place to measure it.
* **Redis semantics.** Everything in the Redis column is from Redis's documented behaviour, not from a
  PersistAnt backend; none of it is tested here.
* Whether `AuthStorage`'s `supports_atomic_swap` exists somewhere this tree does not show (a sibling crate or
  an unmerged branch): grep of auth-framework's `*.rs` finds nothing.

## 6. Order of work, if routed

1. `kv.take`: trait method with the default over `compare_and_delete`, conformance test, matrix row,
   `DESIGN-KINDS.md` note. Smallest; also fixes the `get`-then-`delete` consume in `par.rs` for any consumer.
2. `kv.count`: trait method, native on memory and the four SQL engines, matrix rows, the paged helper.
3. `persistant_kv::index::Indexed`: the layer, its conformance test (a crash between the two writes of an
   update leaves both or neither; concurrent `replace` has one winner), matrix leaf `kv.index`.
4. auth-framework side (its lane): an `AuthStorage` over `Indexed<KvOnRelational<..>>`, known-answer tests
   against `SqliteStorage` for the same operations.

Each is a version change under the portfolio rule; the PM allocates the numbers. Nothing here is implemented.
