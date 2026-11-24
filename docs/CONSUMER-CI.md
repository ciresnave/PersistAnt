# Checking your own backend in CI

ROADMAP row 18, `DESIGN-KINDS` 2.3 (b). The checked-in matrix says what PersistAnt measured on **its** CI
servers. Your production server can differ (a version, a managed service, a setting). This check runs the
key-value suite against *your* server and fails the build if a leaf you need is not there.

## The command

From a checkout of PersistAnt at the tag of the version you depend on (`v<version>`):

```sh
PERSISTANT_CI_URL=postgres://user:pw@host/scratch_db \
PERSISTANT_NEEDS=kv.get,kv.put,kv.put_if_absent,kv.ttl \
  cargo run -q -p persistant-sqlx --example consumer_ci
```

`sqlite:<path>` works too (add `--features sqlite`). Output is one line per needed leaf:

```
ok       kv.get
MISSING  kv.bounded_call: Some(Missing); no call timeout configured
UNKNOWN  kv.putt: not a key-value leaf this suite runs
```

| Exit | Meaning |
|---|---|
| 0 | every needed leaf passed on this backend |
| 1 | a needed leaf did not (its reason is printed) |
| 2 | usage, cannot connect, or a needed id is not a key-value leaf the suite runs: a typo fails the build, it never passes silently |

## A GitHub Actions job

```yaml
backend-check:
  runs-on: ubuntu-latest
  services:
    postgres:
      image: postgres:16        # the version you run in production
      env: { POSTGRES_PASSWORD: ci }
      ports: ["5432:5432"]
      options: >-
        --health-cmd "pg_isready" --health-interval 5s --health-retries 10
  steps:
    - uses: actions/checkout@v4
      with: { repository: ciresnave/PersistAnt, ref: v0.18.33 }   # your persistant version
    - uses: dtolnay/rust-toolchain@stable
    - run: cargo run -q -p persistant-sqlx --example consumer_ci
      env:
        PERSISTANT_CI_URL: postgres://postgres:ci@localhost:5432/postgres
        PERSISTANT_NEEDS: kv.get,kv.put,kv.put_if_absent,kv.ttl
```

## Limits (read these)

* **Key-value leaves only.** Blob and relational leaves are not run by this command; the matrix covers them.
* **It writes.** The suite creates and drops its own `persistant_kv` tables in the database you name. Use a
  scratch database or a staging copy; never data you need.
* **A pass is for that server and that run.** A different version or setting is a different run. It says
  nothing about power loss, disk-full behaviour or concurrency beyond what each leaf's test states
  (`docs/DESIGN-DURABILITY-MACHINE.md`).
* **MySQL and MariaDB** take a `mysql://` URL and a build with `--features mysql` (`cargo run -q -p persistant-sqlx --features mysql --example consumer_ci`); CI runs it on both servers.
* Run against a *production-like* server, not a container of another major version, or the pass is about
  the container.
