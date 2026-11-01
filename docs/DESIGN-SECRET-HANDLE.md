# Design note: secret handle for connection config (ROADMAP "Kinds" row 9)

Status: **proposal, no code.** Written 2026-11-01 at `e92491b` (0.16.2) on the PM's ruling (row 9 next, design only).
Serves `DESIGN-KINDS.md` section 5 ("Secret-handle connection config") and section 10.

## 1. What exists

`persistant_relational::ConnectionSecret` (`crates/persistant-relational/src/secret.rs`) has three variants:
`Env(name)`, `File(path)` and `Literal(Redacted)`. `Redacted` prints `<redacted>` in `Debug` and `Display`;
`expose()` is the one call that reveals the text. `Endpoint::Secret` / `Endpoint::url_env` (`src/registry.rs`)
carry it into the registry; `PgBackend::connect_with` and the MySQL equivalents resolve it at open.
So the row's first two bullets (env var, file, redacted `Debug`) are **built**. What row 9 still asks:
"beyond `ConnectionSecret`", i.e. a `with-secret` reference (DESIGN-KINDS 5, 10) and anything that makes
the handle usable by more than relational DSNs.

## 2. The consumer

`with-secret` (repo `consent`, `crates/with-secret`) is the portfolio's secret release tool. Read from its
source (`main.rs:134-141`, `run.rs`): it runs **one command** with the named secret in **that child's
environment only** (`.env(&secret.env_var, &secret.value)`), after a person approves (Windows Hello, or a
security key on Linux), masks the value in the child's output, and refuses env-dump commands. A secret in its
vault has a name (UPPER_SNAKE), an `env_var`, an access (read/write) and an optional lane list.

The "vault move out of OverMind" (the PM's wording) is the consumer I was pointed at; I did **not** find a
document describing that move (searched `consent/`, `OverMind/` tracked files and the portfolio roadmaps by
the word "vault"; found only the with-secret vault itself and DECISIONS entries that use it). So what the move
changes for this handle is **not verified**.

## 3. Finding: the `Env` variant already is the with-secret integration for a launched program

If a program is started as `with-secret TJ_PROD_DATABASE_URL --reason ... -- ./app`, the vault entry's
`env_var` (for example `DATABASE_URL`) is in that process's environment, and `ConnectionSecret::Env("DATABASE_URL")`
reads it. No PersistAnt change is needed for that path, and the value never touches argv, a file or a log.
It is also the only path that keeps with-secret's guarantee as designed (approval per run, masking, audit).

## 4. Options for a library-level `with-secret` reference

| Option | What it is | For | Against |
|---|---|---|---|
| A. Do nothing | `Env` is the integration (section 3). Document it in the guide with the exact command shape | Zero code, zero new dependency, keeps approval and masking in with-secret | A program not launched by with-secret cannot name a vault entry |
| B. `ConnectionSecret::Named(String)` resolved by shelling out to `with-secret` | The library runs the CLI at open | The program names the entry | Open blocks on a human approval (default wait 540 s); the child must print the value, which with-secret masks, so it cannot be read back at all unless a new `print` mode is added to with-secret (which defeats its purpose); runs a process from a library; no Linux headless path |
| C. A resolver trait: `ConnectionSecret::Custom(Arc<dyn SecretSource>)` | PersistAnt defines `SecretSource { fn resolve(&self) -> Result<Redacted, RelError> }`; the consumer or a future SecretAnt adapter implements it | Keeps PersistAnt free of any vault; `Debug` prints the source's `label()` only; also serves non-relational handles | A trait object in a `Clone + PartialEq + Eq` enum needs a manual `PartialEq` (by label) and `Clone` via `Arc`; one more public API to freeze |
| D. Move the handle to `persistant-core` as a kind-neutral `Secret` | `ConnectionSecret` and `Redacted` become core types, re-exported from relational | Redis/other kinds (auth-framework's `rediss://`) get the same handle; no kind depends on `persistant-relational` for a secret | Breaking move (re-export keeps source compatibility, not the matrix); only worth it with a second user |

**Recommendation: A now, C when a consumer asks, D together with C.** A costs a paragraph in the guide and
fixes the actual gap (nobody knows `Env` is the with-secret path). B is rejected for the reasons in its row,
the first of which is a property of with-secret, not of this crate. C is the honest way to support a vault
that is not with-secret (SecretAnt is a reserved name with nothing implemented), but there is no consumer
asking, so building it now would be a guess.

## 5. Small hardening that does not need a ruling

1. `Redacted` has no `Drop` zeroing and `expose()` returns `&str` into a `String`: a resolved DSN lives in
   ordinary heap memory until freed. State this limit in the doc comment; zeroizing (the `zeroize` crate)
   is a dependency decision, not taken here.
2. `Endpoint::Url(String)` **does** print the URL: `Endpoint` derives `Debug` (`src/registry.rs:90`) and the
   variant's own doc comment says it is "a `Debug` print away from a log file". So `Url` is a documented
   leak by design; the question is whether to deprecate it in favour of `Secret` (a Breaking change, left
   to the PM). Read at `e92491b`, not run.
3. Error text names the variable or file and never a value (tested in `secret.rs`); the sqlx error text from a
   failed connect may include the host and user. **Not verified** that it never includes the password.

## 6. What I did not verify

- The vault move out of OverMind (section 2).
- That with-secret's Linux headless path (every ask `Unavailable`) matters to any PersistAnt consumer.
- Whether any consumer holds a DSN in a `Debug`-printable struct today (DESIGN-KINDS 5 lists Humboldt's
  `expect` on `DATABASE_URL` and auth-framework's `StorageConfig::Postgres { connection_string }`; both
  were "by reading" there and I re-read neither).
- Nothing in this note was built or run; section 3 is from with-secret's source, not from running it.

## 7. Asks

- PM: confirm A (guide paragraph plus this note) as the row-9 outcome, or rule C/D now.
- PM: rule on deprecating `Endpoint::Url` (section 5.2).
- PM: point me at the document for the vault move, if one exists.

## 9. Rulings (PM, 2026-11-01)

1. **Outcome A confirmed** (guide paragraph plus this note). C (`SecretSource` resolver) and D (kind-neutral `Secret` in `persistant-core`) are *scheduled: when a consumer asks*, not declined. B stays rejected.
2. **Deprecating `Endpoint::Url`: not now.** 0.16.4 redacted its `Debug`; `#[deprecated]` fails `-D warnings` consumers. It goes with the next Breaking release, with a migration line (ROADMAP row 23).
3. **`zeroize`:** when taken, a normal current third-party dependency; not now.
4. The vault move is OverMind ROADMAP row 6b (found by the PM).
5. **5.3 was tested (0.16.7):** a failed connect to PostgreSQL or MySQL, with a canary password, shows it in no `Display`, `Debug` or `source()`; mutation-checked. Still not tested: a server's own error text for a wrong password (checked only where the CI servers exist) and the registry's open errors.
