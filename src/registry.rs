// SPDX-License-Identifier: MIT OR Apache-2.0
//! **The registry: needs in, sorted backends out, the choice stored in config, backend-agnostic calls.**
//!
//! A program's setup UI lists the capabilities it needs and the ones that matter most; [`providers`]
//! returns every backend compiled into this build that has all the needed ones, best first, each with
//! the reasons it ranks where it does ([`Listing::why`]). The operator picks one; the program stores
//! its [`Listing::id`] (and the endpoint) in **its own** configuration; at run time [`open_named`]
//! turns that id back into a [`Handle`], after checking the gate again.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use persistant::kv::KvBackend;
//! use persistant::registry::{self, Endpoint};
//! use persistant::capability::{CapabilityId, choose::Durability, provider::ProviderNeeds};
//! let id = |s| CapabilityId::new(s).unwrap();
//! let needs = ProviderNeeds::new(vec![id("kv.get"), id("kv.put")], Durability::Process)
//!     .prefer(vec![id("kv.ttl")]);
//! let list = registry::providers(&needs)?;                    // 1. sorted list for the setup UI
//! let picked = &list[0];                                      // 2. the operator picks one ...
//! // 3. ... and the program stores (picked.id, endpoint) in its own config.
//! let handle = registry::open_named(picked.id, &Endpoint::path("/var/lib/app/kv"), &needs).await?; // 4.
//! handle.kv().put("k", b"v".to_vec(), None).await?;           // 5. backend-agnostic calls only
//! # Ok(()) }
//! ```
//!
//! # Stable ids
//! A [`Listing::id`] is a plain string a program stores in its configuration. **Ids are append-only
//! and never change meaning**: a changed meaning is a new id (the list is pinned by a test). The
//! version of the driver or server is not part of the id; the gate checks it ([`open_named`]
//! re-reads the recorded matrix for the backend it actually opened and refuses if it is not on
//! it). The ids today: `memory`, `fs`, `sqlite`, `sqlite-full`.
//!
//! # What a build can offer, and when a rebuild is needed
//! The table is **compiled in** from the enabled features (`registry` gives `memory` and `fs`;
//! `registry-sqlite` adds `sqlite` and `sqlite-full`; `registry-postgres` adds `postgres`; `registry-mysql` adds `mysql`
//! and `mariadb`; `all-backends` is every one). A new backend
//! module reaches a program when it upgrades `persistant` and **rebuilds**; there are no dynamic
//! plugins. A program that pins an old `persistant` never sees it. Which backends a binary can offer
//! is a fact of that binary, not a runtime accident.
//!
//! # What the handle serves
//! This release's [`Handle`] serves the key-value kind only, so a provider qualifies on `kv.*`
//! capabilities (and `lock.*` is not offered): a need for any other id is reported unmet rather than
//! promised and not delivered.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use persistant_core::choose::{Declared, Durability, Need};
use persistant_core::provider::{
    self, EndpointSpec, Provider, ProviderNeeds, Sorted, Surface, Unmet,
};
use persistant_core::{CapabilityId, Config, Matrix};
use persistant_kv::{BatchOp, BoundedProbe, KeyPage, KvBackend, KvError, KvOnBlob, ValuePage};
use persistant_opendal::OpendalBlob;

#[cfg(any(
    feature = "registry-sqlite",
    feature = "registry-postgres",
    feature = "registry-mysql"
))]
use persistant_kv::KvOnRelational;
use persistant_relational::ConnectionSecret;
#[cfg(any(
    feature = "registry-sqlite",
    feature = "registry-postgres",
    feature = "registry-mysql"
))]
use persistant_sqlx::Bounded;
#[cfg(feature = "registry-mysql")]
use persistant_sqlx::MysqlBackend;
#[cfg(feature = "registry-postgres")]
use persistant_sqlx::PgBackend;
#[cfg(feature = "registry-sqlite")]
use persistant_sqlx::SqliteBackend;

/// Where a provider is opened: what [`EndpointSpec`] asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Endpoint {
    /// Nothing (`memory`).
    None,
    /// A directory or file on the local filesystem.
    Path(PathBuf),
    /// A connection URL held as plain text. Prefer [`Endpoint::Secret`]: a URL usually carries a
    /// password and this one is a `Debug` print away from a log file.
    Url(String),
    /// A connection URL held as a secret: read from the environment, a file or a literal only when
    /// the backend is opened, and never printed.
    Secret(ConnectionSecret),
}

impl Endpoint {
    /// A filesystem endpoint.
    pub fn path(p: impl AsRef<Path>) -> Self {
        Endpoint::Path(p.as_ref().to_path_buf())
    }

    /// A connection URL read from the environment variable `name` when the backend is opened.
    pub fn url_env(name: impl Into<String>) -> Self {
        Endpoint::Secret(ConnectionSecret::Env(name.into()))
    }

    fn kind(&self) -> &'static str {
        match self {
            Endpoint::None => "none",
            Endpoint::Path(_) => "path",
            Endpoint::Url(_) | Endpoint::Secret(_) => "url",
        }
    }

    /// The URL text of a URL-kind endpoint, resolved now. A failure to resolve never carries the URL.
    fn url(&self) -> Result<Option<String>, OpenError> {
        match self {
            Endpoint::Url(u) => Ok(Some(u.clone())),
            Endpoint::Secret(sec) => sec
                .resolve()
                .map(|r| Some(r.expose().to_string()))
                .map_err(|e| OpenError::Open(format!("could not read the connection URL: {e}"))),
            _ => Ok(None),
        }
    }

    fn fits(&self, spec: &EndpointSpec) -> Result<bool, OpenError> {
        Ok(match (self, spec) {
            (Endpoint::None, EndpointSpec::None) => true,
            (Endpoint::Path(_), EndpointSpec::Path) => true,
            (Endpoint::Url(_) | Endpoint::Secret(_), EndpointSpec::Url { schemes }) => self
                .url()?
                .and_then(|u| u.split_once(':').map(|(s, _)| s.to_string()))
                .is_some_and(|s| schemes.contains(&s)),
            _ => false,
        })
    }
}

/// Why [`open_named`] did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenError {
    /// No such id in this build. `known` is every id this build offers.
    UnknownBackend {
        /// The id asked for.
        id: String,
        /// The ids this build can open.
        known: Vec<&'static str>,
    },
    /// The endpoint is not the kind the provider opens from. Nothing was touched.
    WrongEndpoint {
        /// The provider.
        id: &'static str,
        /// What it needs.
        wanted: EndpointSpec,
        /// What was passed (`none`, `path`, `url`).
        got: &'static str,
    },
    /// The gate refused: the unmet capabilities, per backend (here, the one asked for).
    Unmet(Unmet),
    /// The needs include `kv.bounded_call` but no `call_bound` was given. Nothing was touched.
    BoundRequired {
        /// The id asked for.
        id: &'static str,
    },
    /// A `call_bound` was given for a backend that cannot be bounded (`memory`, `fs`).
    BoundUnsupported {
        /// The id asked for.
        id: &'static str,
    },
    /// The server answered as a different backend than the id asked for (a MariaDB server behind
    /// the `mysql` id, or the reverse). Nothing was written.
    ServerMismatch {
        /// The id asked for.
        id: &'static str,
        /// What the server turned out to be (the matrix backend name).
        found: String,
    },
    /// The directory is on a filesystem that ignores case, so two keys that differ only by case
    /// (`Key` and `key`) would be one file and one value. `fs` keeps a key as a file path and
    /// refuses to open there; no key was written (the directory itself may have been created, and is left). Choose a backend that keeps keys
    /// apart, or put the directory on a case-sensitive filesystem.
    CaseInsensitiveFilesystem {
        /// The id asked for.
        id: &'static str,
        /// The directory that was probed.
        path: PathBuf,
    },
    /// The backend failed to open.
    Open(String),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::UnknownBackend { id, known } => {
                write!(
                    f,
                    "unknown backend id `{id}`; this build offers: {}",
                    known.join(", ")
                )
            }
            OpenError::WrongEndpoint { id, wanted, got } => {
                write!(f, "backend `{id}` opens from {wanted:?}, not a {got}")
            }
            OpenError::Unmet(u) => write!(f, "{u}"),
            OpenError::BoundRequired { id } => write!(
                f,
                "backend `{id}`: the needs include kv.bounded_call, which holds only when opened with OpenOptions::call_bound"
            ),
            OpenError::BoundUnsupported { id } => {
                write!(f, "backend `{id}` cannot be opened with a call bound")
            }
            OpenError::ServerMismatch { id, found } => {
                write!(
                    f,
                    "backend `{id}` was asked for, but the server is `{found}`"
                )
            }
            OpenError::CaseInsensitiveFilesystem { id, path } => write!(
                f,
                "backend `{id}` cannot be used at {}: the filesystem there ignores case, so keys that differ only by case would share one file and one value. Choose `sqlite`, `sqlite-full`, `postgres`, `mysql` or `mariadb` (each needs its `registry-*` Cargo feature), or a directory on a case-sensitive filesystem",
                path.display()
            ),
            OpenError::Open(m) => write!(f, "could not open: {m}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Options for [`open_named_with`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenOptions {
    /// Cut every call off after this long, returning a timeout error instead of waiting
    /// (`persistant_sqlx::Bounded`). Needed for `kv.bounded_call`; only the database backends can
    /// be bounded.
    pub call_bound: Option<Duration>,
}

impl OpenOptions {
    /// No options: the defaults ([`open_named`] uses these).
    pub fn new() -> Self {
        Self::default()
    }

    /// Bound every call at `bound`.
    #[must_use]
    pub fn call_bound(mut self, bound: Duration) -> Self {
        self.call_bound = Some(bound);
        self
    }
}

/// How a provider is described in the table: id, words for the operator, and where to look it up.
struct Spec {
    id: &'static str,
    title: &'static str,
    /// The kv-level backend name in the matrix and in the durability table.
    kv_backend: &'static str,
    /// Config facts the recorded rows must carry.
    facts: &'static [(&'static str, &'static str)],
    endpoint: fn() -> EndpointSpec,
    /// Plain-language notes shown with the provider, caveats included.
    notes: &'static [&'static str],
}

fn specs() -> Vec<Spec> {
    #[allow(
        unused_mut,
        reason = "v is pushed to only when a registry-sqlite, -postgres or -mysql feature is enabled"
    )]
    let mut v = vec![
        Spec {
            id: "memory",
            title: "In-process memory",
            kv_backend: "kv-on-blob:opendal-memory",
            facts: &[("put_mode", "atomic")],
            endpoint: || EndpointSpec::None,
            notes: &["Lost when the process exits: for tests and caches only."],
        },
        Spec {
            id: "fs",
            title: "Files in a local directory",
            kv_backend: "kv-on-blob:opendal-fs",
            facts: &[("atomic_write_dir", "set"), ("put_mode", "atomic")],
            endpoint: || EndpointSpec::Path,
            notes: &[
                "Opens from a directory path; writes go through a scratch directory beside it (`<dir>.persistant-atomic`) so a reader never sees a half-written value.",
                "Survives the process exiting or crashing; no fsync is asserted, so a power cut can lose recent writes.",
                "Refuses to open in a directory on a filesystem that ignores case (the Windows and macOS defaults): a key is a file path, so `Key` and `key` would be one value. The check is made at open, on the directory itself, not assumed from the operating system.",
            ],
        },
    ];
    #[cfg(feature = "registry-sqlite")]
    {
        v.push(Spec {
            id: "sqlite",
            title: "SQLite file (default durability)",
            kv_backend: "kv-on-relational:sqlite",
            facts: &[("dialect", "sqlite")],
            endpoint: || EndpointSpec::Path,
            notes: &[
                "Survives the process exiting or crashing. SQLite's `synchronous` setting is not asserted here, so after a power cut the most recent writes may be lost.",
                "Pick `sqlite-full` instead if the data must survive a power cut.",
            ],
        });
        v.push(Spec {
            id: "sqlite-full",
            title: "SQLite file, synchronous=FULL (survives power loss)",
            kv_backend: "kv-on-relational:sqlite-full",
            facts: &[("dialect", "sqlite")],
            endpoint: || EndpointSpec::Path,
            notes: &[
                "Same storage as `sqlite`, opened with journal_mode=WAL and synchronous=FULL set explicitly: each acknowledged write is flushed before it returns. Slower writes than `sqlite`.",
                "Verified by powering off a Hyper-V guest after acknowledged writes (nothing lost); NOT verified for host power loss, NTFS or the disk's own cache.",
            ],
        });
    }
    #[cfg(feature = "registry-postgres")]
    v.push(Spec {
        id: "postgres",
        title: "PostgreSQL server",
        kv_backend: "kv-on-relational:postgres",
        facts: &[("dialect", "postgres")],
        endpoint: || EndpointSpec::Url {
            schemes: vec!["postgres".to_string(), "postgresql".to_string()],
        },
        notes: &[
            "Opens from a connection URL (`postgres://...`); pass it as a secret (environment variable or file) so it is never printed.",
            "Offered only on the server versions the recorded matrix covers; any other server version is refused at open with the unmet ids.",
            "A full disk fails loud (a write returns an error and every acknowledged key is kept).",
        ],
    });
    #[cfg(feature = "registry-mysql")]
    {
        v.push(Spec {
            id: "mysql",
            title: "MySQL server",
            kv_backend: "kv-on-relational:mysql",
            facts: &[("dialect", "mysql")],
            endpoint: || EndpointSpec::Url {
                schemes: vec!["mysql".to_string()],
            },
            notes: &[
                "Opens from a connection URL (`mysql://...`); pass it as a secret. A MariaDB server behind this id is refused: use `mariadb`.",
                "Offered only on the server versions the recorded matrix covers.",
                "CAVEAT: a full disk makes MySQL STALL, not fail: the call gets no answer and no error. Give every call your own timeout (the backend reports `call_bound`).",
            ],
        });
        v.push(Spec {
            id: "mariadb",
            title: "MariaDB server",
            kv_backend: "kv-on-relational:mariadb",
            facts: &[("dialect", "mariadb")],
            endpoint: || EndpointSpec::Url {
                schemes: vec!["mysql".to_string(), "mariadb".to_string()],
            },
            notes: &[
                "Opens from a connection URL (`mysql://...` or `mariadb://...`); pass it as a secret. A MySQL server behind this id is refused: use `mysql`.",
                "Offered only on the server versions the recorded matrix covers.",
                "No durability class is declared for MariaDB (no power-loss run exists for it), so a need above Volatile does not list it.",
            ],
        });
    }
    v
}

/// The ids this build offers, in table order. Append-only: see the module docs.
pub fn known_ids() -> Vec<&'static str> {
    specs().iter().map(|s| s.id).collect()
}

fn matrix() -> &'static Matrix {
    static M: OnceLock<Matrix> = OnceLock::new();
    M.get_or_init(|| {
        #[allow(
            unused_mut,
            reason = "m is merged into only when a registry-sqlite, -postgres or -mysql feature is enabled"
        )]
        let mut m = Matrix::parse(include_str!("../matrix/opendal.tsv"))
            .expect("embedded matrix/opendal.tsv");
        #[cfg(feature = "registry-sqlite")]
        m.merge(
            Matrix::parse(include_str!("../matrix/sqlx-sqlite.tsv"))
                .expect("embedded matrix/sqlx-sqlite.tsv"),
        );
        #[cfg(feature = "registry-postgres")]
        m.merge(
            Matrix::parse(include_str!("../matrix/sqlx-postgres.tsv"))
                .expect("embedded matrix/sqlx-postgres.tsv"),
        );
        #[cfg(feature = "registry-mysql")]
        for text in [
            include_str!("../matrix/sqlx-mysql.tsv"),
            include_str!("../matrix/sqlx-mariadb.tsv"),
        ] {
            m.merge(Matrix::parse(text).expect("embedded mysql/mariadb matrix"));
        }
        m
    })
}

/// `config` with the `call_bound` fact set to `set` or `unset` (the gate reads it at open).
fn with_bound(config: &Config, bounded: bool) -> Config {
    let kept: Vec<String> = config
        .to_string()
        .split(';')
        .filter(|kv| !kv.is_empty() && !kv.starts_with("call_bound="))
        .map(str::to_string)
        .collect();
    let mut c = Config::parse(&kept.join(";")).expect("config facts parse");
    c = c.with("call_bound", if bounded { "set" } else { "unset" });
    c
}

/// The matrix row (version and config) for a spec, taken from the recorded rows, with the
/// `call_bound` fact the program will open with. No row: an empty version, so every lookup is "no
/// row" and the unmet list says so.
fn surface_for(s: &Spec, matrix: &Matrix, bounded: bool) -> Surface {
    let row = matrix.rows().iter().find(|r| {
        r.backend == s.kv_backend && s.facts.iter().all(|(k, v)| r.config.get(k) == Some(v))
    });
    match row {
        Some(r) => Surface::new(
            s.kv_backend,
            r.version.clone(),
            with_bound(&r.config, bounded),
        ),
        None => {
            let mut c = Config::new();
            for (k, v) in s.facts {
                c = c.with(*k, *v);
            }
            Surface::new(s.kv_backend, "", with_bound(&c, bounded))
        }
    }
}

fn provider_for(s: &Spec, matrix: &Matrix, bounded: bool) -> Provider {
    Provider::new(
        s.id,
        vec![surface_for(s, matrix, bounded)],
        persistant_kv::choose::declared_durability(s.kv_backend),
        (s.endpoint)(),
    )
}

fn table(matrix: &Matrix, bounded: bool) -> Vec<Provider> {
    specs()
        .iter()
        .map(|s| provider_for(s, matrix, bounded))
        .collect()
}

/// Whether the needs include `kv.bounded_call`: the program wants every call cut off at a bound.
fn wants_bound(needs: &ProviderNeeds) -> bool {
    needs.ids().iter().any(|i| i.as_str() == "kv.bounded_call")
}

/// One backend in the sorted list a setup UI shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// The stable id to store in the program's configuration.
    pub id: &'static str,
    /// A name for the operator.
    pub title: &'static str,
    /// What it needs to be opened.
    pub endpoint: EndpointSpec,
    /// Its declared durability class, the basis, and how much a test has verified.
    pub durability: Option<Declared>,
    /// Each preferred capability, in the order of importance given, and whether this backend has it.
    pub preferred: Vec<(CapabilityId, bool)>,
    /// The matrix version the capability rows were recorded under (adapter version, then the
    /// driver or server version after the `+`). The backend is refused at open on any other.
    pub recorded: String,
    /// Measured median time of the ranking workload, if one was supplied and measured.
    pub p50_ns: Option<u64>,
    /// Caveats in plain words (what it does not survive, what to choose instead).
    pub notes: Vec<&'static str>,
}

impl Listing {
    /// Why this backend is here and ranks where it does, as lines for the setup UI: the preferred
    /// capabilities it meets and lacks (in order of importance), the speed, the durability with its
    /// basis, and the caveats.
    pub fn why(&self) -> Vec<String> {
        let mut out = Vec::new();
        let join = |met: bool| {
            self.preferred
                .iter()
                .filter(|(_, m)| *m == met)
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        if !self.preferred.is_empty() {
            out.push(format!(
                "preferred, in your order of importance: meets [{}]; lacks [{}]",
                join(true),
                join(false)
            ));
        }
        if let Some((_, server)) = self.recorded.split_once('+') {
            out.push(format!("recorded on version {server} only"));
        }
        match self.p50_ns {
            Some(ns) => out.push(format!("speed: median {ns} ns on the ranking workload")),
            None => out.push("speed: not measured for the ranking workload here".to_string()),
        }
        match self.durability {
            Some(d) => out.push(format!(
                "durability: {} (verified: {}); {}",
                d.class.as_str(),
                d.verified.map_or("nothing", Durability::as_str),
                d.basis
            )),
            None => out.push("durability: not declared".to_string()),
        }
        out.extend(self.notes.iter().map(|n| (*n).to_string()));
        out
    }
}

fn listing(s: &Sorted, bounded: bool) -> Listing {
    let spec = specs()
        .into_iter()
        .find(|x| x.id == s.provider.name())
        .expect("a sorted provider came from the table");
    Listing {
        id: spec.id,
        title: spec.title,
        endpoint: s.provider.endpoint().clone(),
        durability: s.provider.declared(),
        preferred: s.preferred.clone(),
        recorded: s.provider.primary().version.clone(),
        p50_ns: s.p50_ns,
        notes: {
            let mut n = spec.notes.to_vec();
            if bounded {
                n.push(
                    "kv.bounded_call: holds only when opened with a call bound (OpenOptions::call_bound); open_named refuses the open without one.",
                );
            }
            n
        },
    }
}

/// Every backend this build offers that has **all** of `needs`, sorted: by the preferred
/// capabilities in order of importance, then by name. `Err(Unmet)` names, for each backend, what it
/// lacks; there is no closest match. For a measured speed order use [`providers_ranked`].
pub fn providers(needs: &ProviderNeeds) -> Result<Vec<Listing>, Unmet> {
    providers_ranked(needs, None)
}

/// As [`providers`], with the workload and environment to order by after the preferred
/// capabilities, and the recorded speed table: `speed = Some((need, table))`.
pub fn providers_ranked(
    needs: &ProviderNeeds,
    speed: Option<(&Need, &persistant_core::bench::SpeedTable)>,
) -> Result<Vec<Listing>, Unmet> {
    let m = matrix();
    let bounded = wants_bound(needs);
    let sorted = provider::select(&table(m, bounded), needs, m, speed)?;
    Ok(sorted.iter().map(|s| listing(s, bounded)).collect())
}

/// A key-value backend of whichever provider was opened. A closed set: the build decides which
/// variants exist. It implements [`KvBackend`], forwarding every method, so a program's calls do not
/// name a backend.
#[derive(Debug)]
#[non_exhaustive]
pub enum AnyKv {
    /// `memory` and `fs`.
    Blob(KvOnBlob<OpendalBlob>),
    /// `sqlite` and `sqlite-full`.
    #[cfg(feature = "registry-sqlite")]
    Sqlite(KvOnRelational<SqliteBackend>),
    /// `sqlite` and `sqlite-full`, opened with a call bound.
    #[cfg(feature = "registry-sqlite")]
    SqliteBounded(KvOnRelational<Bounded<SqliteBackend>>),
    /// `postgres`.
    #[cfg(feature = "registry-postgres")]
    Postgres(KvOnRelational<PgBackend>),
    /// `postgres`, opened with a call bound.
    #[cfg(feature = "registry-postgres")]
    PostgresBounded(KvOnRelational<Bounded<PgBackend>>),
    /// `mysql` and `mariadb`.
    #[cfg(feature = "registry-mysql")]
    Mysql(KvOnRelational<MysqlBackend>),
    /// `mysql` and `mariadb`, opened with a call bound.
    #[cfg(feature = "registry-mysql")]
    MysqlBounded(KvOnRelational<Bounded<MysqlBackend>>),
}

macro_rules! each {
    ($self:ident, $k:ident => $e:expr) => {
        match $self {
            AnyKv::Blob($k) => $e,
            #[cfg(feature = "registry-sqlite")]
            AnyKv::Sqlite($k) => $e,
            #[cfg(feature = "registry-sqlite")]
            AnyKv::SqliteBounded($k) => $e,
            #[cfg(feature = "registry-postgres")]
            AnyKv::Postgres($k) => $e,
            #[cfg(feature = "registry-postgres")]
            AnyKv::PostgresBounded($k) => $e,
            #[cfg(feature = "registry-mysql")]
            AnyKv::Mysql($k) => $e,
            #[cfg(feature = "registry-mysql")]
            AnyKv::MysqlBounded($k) => $e,
        }
    };
}

impl KvBackend for AnyKv {
    fn backend(&self) -> &str {
        each!(self, k => k.backend())
    }
    fn version(&self) -> &str {
        each!(self, k => k.version())
    }
    fn call_bound(&self) -> Option<Duration> {
        each!(self, k => k.call_bound())
    }
    async fn bounded_call_probe(&self) -> Result<BoundedProbe, KvError> {
        each!(self, k => k.bounded_call_probe().await)
    }
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        each!(self, k => k.get(key).await)
    }
    async fn put(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<(), KvError> {
        each!(self, k => k.put(key, value, ttl).await)
    }
    async fn delete(&self, key: &str) -> Result<(), KvError> {
        each!(self, k => k.delete(key).await)
    }
    async fn put_if_absent(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        each!(self, k => k.put_if_absent(key, value, ttl).await)
    }
    async fn prefix_list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        each!(self, k => k.prefix_list(prefix, after, limit).await)
    }
    async fn ordered_scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        each!(self, k => k.ordered_scan(prefix, after, limit).await)
    }
    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &[u8],
        new: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        each!(self, k => k.compare_and_swap(key, expected, new, ttl).await)
    }
    async fn compare_and_delete(&self, key: &str, expected: &[u8]) -> Result<bool, KvError> {
        each!(self, k => k.compare_and_delete(key, expected).await)
    }
    async fn count(&self, prefix: &str) -> Result<u64, KvError> {
        each!(self, k => k.count(prefix).await)
    }
    async fn take(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        each!(self, k => k.take(key).await)
    }
    async fn ttl_remaining(&self, key: &str) -> Result<Option<Duration>, KvError> {
        each!(self, k => k.ttl_remaining(key).await)
    }
    async fn purge_expired(&self) -> Result<u64, KvError> {
        each!(self, k => k.purge_expired().await)
    }
    async fn batch(&self, ops: &[BatchOp]) -> Result<bool, KvError> {
        each!(self, k => k.batch(ops).await)
    }
    async fn ordered_scan_values(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ValuePage, KvError> {
        each!(self, k => k.ordered_scan_values(prefix, after, limit).await)
    }
}

/// An opened backend. Use [`Handle::kv`] for backend-agnostic calls.
#[derive(Debug)]
pub struct Handle {
    id: &'static str,
    kv: AnyKv,
}

impl Handle {
    /// The id it was opened under.
    pub fn id(&self) -> &'static str {
        self.id
    }

    /// The key-value store.
    pub fn kv(&self) -> &AnyKv {
        &self.kv
    }

    /// The key-value store, owned: for a consumer that is generic over `K: KvBackend` and keeps the
    /// store (the handle's id is dropped; read it with [`Handle::id`] first if it is wanted).
    pub fn into_kv(self) -> AnyKv {
        self.kv
    }
}

/// Open the backend `id` (the string the program stored in its configuration) at `endpoint`.
///
/// The gate is checked **twice**: before anything is touched (the recorded matrix against `needs`),
/// and again for the backend actually opened (its own name and version, so a driver or library that
/// moved off the recorded rows is refused). Either failure is [`OpenError::Unmet`] with the unmet
/// ids; a wrong kind of endpoint is refused before any I/O.
pub async fn open_named(
    id: &str,
    endpoint: &Endpoint,
    needs: &ProviderNeeds,
) -> Result<Handle, OpenError> {
    open_named_with(id, endpoint, needs, &OpenOptions::new()).await
}

/// As [`open_named`], with [`OpenOptions`]. If the needs include `kv.bounded_call`, a
/// `call_bound` is required ([`OpenError::BoundRequired`], before any I/O); with one, the backend is
/// opened through `persistant_sqlx::Bounded` and the second gate reads the bound back from the
/// backend actually opened.
pub async fn open_named_with(
    id: &str,
    endpoint: &Endpoint,
    needs: &ProviderNeeds,
    options: &OpenOptions,
) -> Result<Handle, OpenError> {
    let m = matrix();
    let all = specs();
    let Some(spec) = all.iter().find(|s| s.id == id) else {
        return Err(OpenError::UnknownBackend {
            id: id.to_string(),
            known: all.iter().map(|s| s.id).collect(),
        });
    };
    if wants_bound(needs) && options.call_bound.is_none() {
        return Err(OpenError::BoundRequired { id: spec.id });
    }
    if options.call_bound.is_some() && matches!(spec.id, "memory" | "fs") {
        return Err(OpenError::BoundUnsupported { id: spec.id });
    }
    let want = (spec.endpoint)();
    if !endpoint.fits(&want)? {
        return Err(OpenError::WrongEndpoint {
            id: spec.id,
            wanted: want,
            got: endpoint.kind(),
        });
    }
    let p = provider_for(spec, m, options.call_bound.is_some());
    provider::providers(std::slice::from_ref(&p), needs, m).map_err(OpenError::Unmet)?;
    let kv = open_kv(spec.id, endpoint, options.call_bound).await?;
    if kv.backend() != spec.kv_backend {
        return Err(OpenError::ServerMismatch {
            id: spec.id,
            found: kv.backend().to_string(),
        });
    }
    // Again, for what was actually opened.
    let actual = Provider::new(
        spec.id,
        vec![Surface::new(
            kv.backend(),
            kv.version(),
            with_bound(&p.primary().config, kv.call_bound().is_some()),
        )],
        persistant_kv::choose::declared_durability(kv.backend()),
        p.endpoint().clone(),
    );
    provider::providers(std::slice::from_ref(&actual), needs, m).map_err(OpenError::Unmet)?;
    Ok(Handle { id: spec.id, kv })
}

fn path_exists(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

/// Whether `dir` is on a filesystem that ignores case: create a probe file whose name has both cases
/// in it, look for the same name with its case flipped, and remove the probe. `exists` is how the
/// flipped name is looked up (the real filesystem in production; a stand-in in the tests). The probe
/// is always removed, and a probe that cannot be created is an error, not "case-sensitive".
fn directory_ignores_case(dir: &Path, exists: fn(&Path) -> bool) -> std::io::Result<bool> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = format!(".Persistant-Case-Probe-{}-{nanos}", std::process::id());
    let flipped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    debug_assert_ne!(name, flipped);
    let probe = dir.join(&name);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    let aliased = exists(&dir.join(&flipped));
    std::fs::remove_file(&probe)?;
    Ok(aliased)
}

/// Open the key-value store of provider `id`. With a `bound`, the database backends are opened
/// through `Bounded`; the tables are created first through an unbounded connection, because the DDL
/// of a first open can outlast any bound short enough to be useful and what is bounded is the
/// program's calls, not setup.
async fn open_kv(
    id: &str,
    endpoint: &Endpoint,
    bound: Option<Duration>,
) -> Result<AnyKv, OpenError> {
    let open = |e: &dyn fmt::Display| OpenError::Open(e.to_string());
    match (id, endpoint) {
        ("memory", _) => Ok(AnyKv::Blob(KvOnBlob::new(
            OpendalBlob::memory().map_err(|e| open(&e))?,
        ))),
        ("fs", Endpoint::Path(root)) => {
            let mut scratch = root.clone().into_os_string();
            scratch.push(".persistant-atomic");
            let scratch = PathBuf::from(scratch);
            std::fs::create_dir_all(root).map_err(|e| open(&e))?;
            if directory_ignores_case(root, path_exists).map_err(|e| open(&e))? {
                return Err(OpenError::CaseInsensitiveFilesystem {
                    id: "fs",
                    path: root.clone(),
                });
            }
            std::fs::create_dir_all(&scratch).map_err(|e| open(&e))?;
            Ok(AnyKv::Blob(KvOnBlob::new(
                OpendalBlob::fs(root, Some(&scratch)).map_err(|e| open(&e))?,
            )))
        }
        #[cfg(feature = "registry-sqlite")]
        ("sqlite" | "sqlite-full", Endpoint::Path(file)) => {
            let durable = id == "sqlite-full";
            let connect = || async {
                if durable {
                    SqliteBackend::open_durable(file).await
                } else {
                    SqliteBackend::open(file).await
                }
                .map_err(|e| open(&e))
            };
            let db = connect().await?;
            let kv = KvOnRelational::open(db).await.map_err(|e| open(&e))?;
            match bound {
                None => Ok(AnyKv::Sqlite(kv)),
                Some(b) => {
                    drop(kv);
                    let db = Bounded::new(connect().await?, b);
                    Ok(AnyKv::SqliteBounded(
                        KvOnRelational::open(db).await.map_err(|e| open(&e))?,
                    ))
                }
            }
        }
        #[cfg(feature = "registry-postgres")]
        ("postgres", Endpoint::Url(_) | Endpoint::Secret(_)) => {
            let url = endpoint.url()?.unwrap_or_default();
            let db = PgBackend::connect(&url).await.map_err(|e| open(&e))?;
            let kv = KvOnRelational::open(db).await.map_err(|e| open(&e))?;
            match bound {
                None => Ok(AnyKv::Postgres(kv)),
                Some(b) => {
                    drop(kv);
                    let db = PgBackend::connect(&url).await.map_err(|e| open(&e))?;
                    Ok(AnyKv::PostgresBounded(
                        KvOnRelational::open(Bounded::new(db, b))
                            .await
                            .map_err(|e| open(&e))?,
                    ))
                }
            }
        }
        #[cfg(feature = "registry-mysql")]
        ("mysql" | "mariadb", Endpoint::Url(_) | Endpoint::Secret(_)) => {
            let url = endpoint.url()?.unwrap_or_default();
            let db = MysqlBackend::connect(&url).await.map_err(|e| open(&e))?;
            let kv = KvOnRelational::open(db).await.map_err(|e| open(&e))?;
            match bound {
                None => Ok(AnyKv::Mysql(kv)),
                Some(b) => {
                    drop(kv);
                    let db = MysqlBackend::connect(&url).await.map_err(|e| open(&e))?;
                    Ok(AnyKv::MysqlBounded(
                        KvOnRelational::open(Bounded::new(db, b))
                            .await
                            .map_err(|e| open(&e))?,
                    ))
                }
            }
        }
        _ => Err(OpenError::Open(format!("no opener for `{id}`"))),
    }
}

#[cfg(test)]
mod case_probe_tests {
    use super::*;

    #[test]
    fn a_stand_in_that_sees_the_flipped_name_means_case_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        assert!(directory_ignores_case(dir.path(), |_| true).unwrap());
    }

    #[test]
    fn a_stand_in_that_does_not_see_it_means_case_is_kept_and_the_probe_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!directory_ignores_case(dir.path(), |_| false).unwrap());
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "the probe file is gone"
        );
        // And with a positive answer too.
        assert!(directory_ignores_case(dir.path(), |_| true).unwrap());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_directory_that_cannot_be_written_is_an_error_not_case_sensitive() {
        let missing = std::env::temp_dir().join("persistant-no-such-dir-for-the-probe/x");
        assert!(directory_ignores_case(&missing, path_exists).is_err());
    }

    #[test]
    fn the_real_lookup_agrees_with_the_operating_system_default() {
        // The temporary directory is on the platform's default filesystem: NTFS ignores case, a
        // Linux tmpfs or ext4 keeps it. macOS volumes can be either, so it is not asserted there.
        let dir = tempfile::tempdir().unwrap();
        let ignores = directory_ignores_case(dir.path(), path_exists).unwrap();
        if cfg!(windows) {
            assert!(ignores, "NTFS ignores case by default");
        } else if cfg!(target_os = "linux") {
            assert!(!ignores, "a Linux temp directory keeps case");
        }
    }
}
