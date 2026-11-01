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
//! This release's [`Handle`] serves the key-value kind (always), the blob kind ([`Handle::blob`],
//! present when the needs name a `blob.*` id) and the relational kind ([`Handle::rel`], present when
//! they name a `rel.*` id). A provider is **listed** on `kv.*`,
//! `blob.*` and `rel.*` capabilities alike: it has one matrix surface per kind it serves (`fs` the
//! blob store under its key-value store, `sqlite` the relational database), the gate needs every id
//! on one of them, and the durability shown is the weakest of the kinds the need names. A need for
//! `lock.*` ids is reported unmet: `lock.*` is not offered.

use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use persistant_blob::{BlobBackend, BlobError, BlobSink, Entry, KeyLayout, Page};
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
use persistant_relational::{
    ConnectionSecret, Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Row, Value,
};
#[cfg(any(
    feature = "registry-sqlite",
    feature = "registry-postgres",
    feature = "registry-mysql"
))]
use persistant_sqlx::{Bounded, BoundedTx};
#[cfg(feature = "registry-mysql")]
use persistant_sqlx::{MysqlBackend, MysqlTx};
#[cfg(feature = "registry-postgres")]
use persistant_sqlx::{PgBackend, PgTx};
#[cfg(feature = "registry-sqlite")]
use persistant_sqlx::{SqliteBackend, SqliteTx};

/// Where a provider is opened: what [`EndpointSpec`] asks for.
///
/// `Debug` never shows a credential: [`Endpoint::Url`] prints with its userinfo, its query values
/// and its fragment replaced by `<redacted>`.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Endpoint {
    /// Nothing (`memory`).
    None,
    /// A directory or file on the local filesystem.
    Path(PathBuf),
    /// A connection URL held as plain text. Prefer [`Endpoint::Secret`]: a URL usually carries a
    /// password. `Debug` redacts the userinfo, the query values and the fragment, but the text is
    /// still in memory, in `Clone`s and in anything that reads the field.
    Url(String),
    /// A connection URL held as a secret: read from the environment, a file or a literal only when
    /// the backend is opened, and never printed.
    Secret(ConnectionSecret),
}

/// A connection URL with everything that can be a credential replaced by `<redacted>`: the
/// userinfo (everything before the last `@` ahead of the query, so a password holding `@` or `/`
/// is covered), the value of every query parameter (the name stays) and the fragment. The scheme,
/// host, port and path stay: they say where, not who. Text that is not `scheme://...` is
/// hidden whole, because it may be a keyword/value connection string with the password in it.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "<redacted>".to_string();
    };
    let (rest, fragment) = match rest.split_once('#') {
        Some((r, _)) => (r, true),
        None => (rest, false),
    };
    let (location, query) = match rest.split_once('?') {
        Some((l, q)) => (l, Some(q)),
        None => (rest, None),
    };
    let location = match location.rsplit_once('@') {
        Some((_, after)) => format!("<redacted>@{after}"),
        None => location.to_string(),
    };
    let mut out = format!("{scheme}://{location}");
    if let Some(q) = query {
        let pairs: Vec<String> = q
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((name, _)) => format!("{name}=<redacted>"),
                None => "<redacted>".to_string(),
            })
            .collect();
        out.push('?');
        out.push_str(&pairs.join("&"));
    }
    if fragment {
        out.push_str("#<redacted>");
    }
    out
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Endpoint::None => f.write_str("None"),
            Endpoint::Path(p) => f.debug_tuple("Path").field(p).finish(),
            Endpoint::Url(u) => f
                .debug_tuple("Url")
                .field(&format_args!("{}", redact_url(u)))
                .finish(),
            Endpoint::Secret(sec) => f.debug_tuple("Secret").field(sec).finish(),
        }
    }
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
    /// **No longer returned** (0.9.42). `fs` once refused a directory on a filesystem that ignores
    /// case, because a key was a file path and `Key` and `key` shared one file. It now writes each
    /// key's bytes as lower-case hex, so no two keys share a file whatever the filesystem; the
    /// variant stays so that existing `match` arms still compile.
    CaseInsensitiveFilesystem {
        /// The id asked for.
        id: &'static str,
        /// The directory that was probed.
        path: PathBuf,
    },
    /// Not returned since 0.11.15 (every kind a provider is listed for is now opened); kept so a
    /// `match` written against 0.11.13 or 0.11.14 still compiles. It meant: the gate passed for a
    /// kind the handle did not yet serve, and nothing was touched.
    KindNotServed {
        /// The id asked for.
        id: &'static str,
        /// The kind.
        kind: &'static str,
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
            OpenError::KindNotServed { id, kind } => write!(
                f,
                "backend `{id}` has every {kind}.* capability asked for, but this release's handle does not serve that kind yet"
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
    /// The blob layer the key-value store sits on, if the provider also serves the blob kind:
    /// its matrix backend name and config facts.
    blob: Option<Layer>,
    /// The relational layer the key-value store sits on, if the provider also serves the relational
    /// kind.
    rel: Option<Layer>,
    endpoint: fn() -> EndpointSpec,
    /// Plain-language notes shown with the provider, caveats included.
    notes: &'static [&'static str],
}

/// A layer a provider serves besides key-value: the matrix backend name and the config facts its rows
/// must carry.
type Layer = (&'static str, &'static [(&'static str, &'static str)]);

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
            blob: Some(("opendal-memory", &[])),
            rel: None,
            endpoint: || EndpointSpec::None,
            notes: &["Lost when the process exits: for tests and caches only."],
        },
        Spec {
            id: "fs",
            title: "Files in a local directory",
            kv_backend: "kv-on-blob:opendal-fs",
            facts: &[("atomic_write_dir", "set"), ("put_mode", "atomic")],
            blob: Some(("opendal-fs", &[("atomic_write_dir", "set")])),
            rel: None,
            endpoint: || EndpointSpec::Path,
            notes: &[
                "Opens from a directory path; writes go through a scratch directory beside it (`<dir>.persistant-atomic`) so a reader never sees a half-written value.",
                "Survives the process exiting or crashing; no fsync is asserted, so a power cut can lose recent writes.",
                "A key is stored under its bytes in lower-case hex (`kvx/` in the directory), so keys that differ only by case stay apart on a filesystem that ignores case (Windows, macOS), whatever the key contains. A directory written by 0.9.41 or earlier is migrated at open.",
                "Needs the directory and its scratch directory on one filesystem with hard links (the exclusive create links a finished scratch file to the key).",
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
            blob: None,
            rel: Some(("sqlite", &[("dialect", "sqlite")])),
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
            blob: None,
            rel: Some(("sqlite-full", &[("dialect", "sqlite")])),
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
        blob: None,
        rel: Some(("postgres", &[("dialect", "postgres")])),
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
            blob: None,
            rel: Some(("mysql", &[("dialect", "mysql")])),
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
            blob: None,
            rel: Some(("mariadb", &[("dialect", "mariadb")])),
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

/// The kinds of capability a need list names, told by the id's prefix: `blob.*`, `rel.*`, and
/// everything else (`kv.*`, `lock.*`) as key-value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Kv,
    Blob,
    Rel,
}

fn kind_of(id: &str) -> Kind {
    if id.starts_with("blob.") {
        Kind::Blob
    } else if id.starts_with("rel.") {
        Kind::Rel
    } else {
        Kind::Kv
    }
}

/// The kinds `needs` names, key-value first; key-value alone for an empty list.
fn kinds_named(needs: &ProviderNeeds) -> Vec<Kind> {
    let mut v: Vec<Kind> = [Kind::Kv, Kind::Blob, Kind::Rel]
        .into_iter()
        .filter(|k| needs.ids().iter().any(|i| kind_of(i.as_str()) == *k))
        .collect();
    if v.is_empty() {
        v.push(Kind::Kv);
    }
    v
}

/// The matrix row (version and config) for a spec's key-value layer, taken from the recorded rows,
/// with the `call_bound` fact the program will open with. No row: an empty version, so every lookup
/// is "no row" and the unmet list says so.
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

/// As [`surface_for`], for a blob or relational layer (no `call_bound` fact: those rows carry none).
fn layer_surface(layer: Layer, matrix: &Matrix) -> Surface {
    let (backend, facts) = layer;
    let row = matrix
        .rows()
        .iter()
        .find(|r| r.backend == backend && facts.iter().all(|(k, v)| r.config.get(k) == Some(v)));
    match row {
        Some(r) => Surface::new(backend, r.version.clone(), r.config.clone()),
        None => {
            let mut c = Config::new();
            for (k, v) in facts {
                c = c.with(*k, *v);
            }
            Surface::new(backend, "", c)
        }
    }
}

/// One surface per kind in `kinds` that the spec serves, in that order (the first is the primary,
/// which speed rows are matched on). A spec that serves none of them gets its key-value surface, so
/// every id of the missing kind is "no row" and the unmet list says so: there is no closest match.
fn surfaces_for(s: &Spec, matrix: &Matrix, bounded: bool, kinds: &[Kind]) -> Vec<(Kind, Surface)> {
    let mut out: Vec<(Kind, Surface)> = kinds
        .iter()
        .filter_map(|k| match k {
            Kind::Kv => Some((Kind::Kv, surface_for(s, matrix, bounded))),
            Kind::Blob => s.blob.map(|l| (Kind::Blob, layer_surface(l, matrix))),
            Kind::Rel => s.rel.map(|l| (Kind::Rel, layer_surface(l, matrix))),
        })
        .collect();
    if out.is_empty() {
        out.push((Kind::Kv, surface_for(s, matrix, bounded)));
    }
    out
}

/// The declared durability of a provider that serves several kinds: the **weakest** class among the
/// surfaces, with that surface's basis. One unknown makes it unknown.
fn weakest_declared(surfaces: &[(Kind, Surface)]) -> Option<Declared> {
    let mut weakest: Option<Declared> = None;
    for (kind, surface) in surfaces {
        let d = match kind {
            Kind::Kv => persistant_kv::choose::declared_durability(&surface.backend),
            Kind::Blob => persistant_blob::choose::declared_durability(&surface.backend),
            Kind::Rel => persistant_relational::choose::declared_durability(&surface.backend),
        }?;
        weakest = match weakest {
            Some(w) if w.class <= d.class => Some(w),
            _ => Some(d),
        };
    }
    weakest
}

fn provider_for(s: &Spec, matrix: &Matrix, bounded: bool, kinds: &[Kind]) -> Provider {
    let surfaces = surfaces_for(s, matrix, bounded, kinds);
    let declared = weakest_declared(&surfaces);
    Provider::new(
        s.id,
        surfaces.into_iter().map(|(_, surface)| surface).collect(),
        declared,
        (s.endpoint)(),
    )
}

fn table(matrix: &Matrix, bounded: bool, kinds: &[Kind]) -> Vec<Provider> {
    specs()
        .iter()
        .map(|s| provider_for(s, matrix, bounded, kinds))
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

fn listing(s: &Sorted, bounded: bool, kinds: &[Kind]) -> Listing {
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
            if kinds.len() > 1 {
                n.push(
                    "The need names more than one kind: the durability shown is the weakest of them, and a speed ranking uses the first kind named.",
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
    let kinds = kinds_named(needs);
    let sorted = provider::select(&table(m, bounded, &kinds), needs, m, speed)?;
    Ok(sorted.iter().map(|s| listing(s, bounded, &kinds)).collect())
}

/// `capant`'s account of provider `id` against `needs`: one line per need, exactly as
/// `Evaluation::explain` renders it (`kv.ttl: yes (probe ..., ...)`), whether or not the provider
/// qualified. `None` for an id this build does not offer. A caller that shows a reason per backend
/// (ConfigurAnt joins these lines with `"; "`) uses this instead of building its own wording.
pub fn explain(needs: &ProviderNeeds, id: &str) -> Option<Vec<String>> {
    let m = matrix();
    let bounded = wants_bound(needs);
    let kinds = kinds_named(needs);
    provider::explain(&table(m, bounded, &kinds), needs, m, id)
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

/// The relational database of whichever provider was opened. A closed set: the build decides which
/// variants exist. It implements [`RelationalBackend`], forwarding every method, so a program's
/// calls do not name a backend; statements are still written in the dialect it reports
/// ([`RelationalBackend::dialect`]).
#[derive(Debug)]
#[non_exhaustive]
pub enum AnyRel {
    /// No relational provider is built into this binary; never constructed.
    #[doc(hidden)]
    Never(std::convert::Infallible),
    /// `sqlite` and `sqlite-full`.
    #[cfg(feature = "registry-sqlite")]
    Sqlite(SqliteBackend),
    /// `sqlite` and `sqlite-full`, opened with a call bound.
    #[cfg(feature = "registry-sqlite")]
    SqliteBounded(Bounded<SqliteBackend>),
    /// `postgres`.
    #[cfg(feature = "registry-postgres")]
    Postgres(PgBackend),
    /// `postgres`, opened with a call bound.
    #[cfg(feature = "registry-postgres")]
    PostgresBounded(Bounded<PgBackend>),
    /// `mysql` and `mariadb`.
    #[cfg(feature = "registry-mysql")]
    Mysql(MysqlBackend),
    /// `mysql` and `mariadb`, opened with a call bound.
    #[cfg(feature = "registry-mysql")]
    MysqlBounded(Bounded<MysqlBackend>),
}

/// A transaction of an [`AnyRel`]. Dropping it without `commit` rolls it back.
#[non_exhaustive]
pub enum AnyTx<'a> {
    /// Never constructed (see [`AnyRel::Never`]); carries the lifetime when no driver is built in.
    #[doc(hidden)]
    Never(std::convert::Infallible, PhantomData<&'a ()>),
    /// A `sqlite` transaction.
    #[cfg(feature = "registry-sqlite")]
    Sqlite(SqliteTx),
    /// A bounded `sqlite` transaction.
    #[cfg(feature = "registry-sqlite")]
    SqliteBounded(BoundedTx<SqliteTx>),
    /// A `postgres` transaction.
    #[cfg(feature = "registry-postgres")]
    Postgres(PgTx<'a>),
    /// A bounded `postgres` transaction.
    #[cfg(feature = "registry-postgres")]
    PostgresBounded(BoundedTx<PgTx<'a>>),
    /// A `mysql` or `mariadb` transaction.
    #[cfg(feature = "registry-mysql")]
    Mysql(MysqlTx),
    /// A bounded `mysql` or `mariadb` transaction.
    #[cfg(feature = "registry-mysql")]
    MysqlBounded(BoundedTx<MysqlTx>),
}

macro_rules! each_rel {
    ($self:ident, $r:ident => $e:expr) => {
        match $self {
            AnyRel::Never(n) => match *n {},
            #[cfg(feature = "registry-sqlite")]
            AnyRel::Sqlite($r) => $e,
            #[cfg(feature = "registry-sqlite")]
            AnyRel::SqliteBounded($r) => $e,
            #[cfg(feature = "registry-postgres")]
            AnyRel::Postgres($r) => $e,
            #[cfg(feature = "registry-postgres")]
            AnyRel::PostgresBounded($r) => $e,
            #[cfg(feature = "registry-mysql")]
            AnyRel::Mysql($r) => $e,
            #[cfg(feature = "registry-mysql")]
            AnyRel::MysqlBounded($r) => $e,
        }
    };
}

macro_rules! each_tx {
    ($self:ident, $t:ident => $e:expr) => {
        match $self {
            // An `Infallible` is behind it, so this arm cannot run; `n` may be a reference or a value
            // depending on the caller, hence no empty `match`.
            AnyTx::Never(..) => unreachable!("AnyTx::Never is never constructed"),
            #[cfg(feature = "registry-sqlite")]
            AnyTx::Sqlite($t) => $e,
            #[cfg(feature = "registry-sqlite")]
            AnyTx::SqliteBounded($t) => $e,
            #[cfg(feature = "registry-postgres")]
            AnyTx::Postgres($t) => $e,
            #[cfg(feature = "registry-postgres")]
            AnyTx::PostgresBounded($t) => $e,
            #[cfg(feature = "registry-mysql")]
            AnyTx::Mysql($t) => $e,
            #[cfg(feature = "registry-mysql")]
            AnyTx::MysqlBounded($t) => $e,
        }
    };
}

#[cfg_attr(
    not(any(
        feature = "registry-sqlite",
        feature = "registry-postgres",
        feature = "registry-mysql"
    )),
    allow(
        unused_variables,
        reason = "with no relational driver built in, every arm but `Never` is compiled out"
    )
)]
impl RelationalBackend for AnyRel {
    type Tx<'a>
        = AnyTx<'a>
    where
        Self: 'a;

    fn backend(&self) -> &str {
        each_rel!(self, r => r.backend())
    }
    fn version(&self) -> &str {
        each_rel!(self, r => r.version())
    }
    fn dialect(&self) -> Dialect {
        each_rel!(self, r => r.dialect())
    }
    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        each_rel!(self, r => r.execute(sql, params).await)
    }
    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        each_rel!(self, r => r.query(sql, params).await)
    }
    async fn begin(&self, isolation: Isolation) -> Result<AnyTx<'_>, RelError> {
        match self {
            AnyRel::Never(n) => match *n {},
            #[cfg(feature = "registry-sqlite")]
            AnyRel::Sqlite(r) => Ok(AnyTx::Sqlite(r.begin(isolation).await?)),
            #[cfg(feature = "registry-sqlite")]
            AnyRel::SqliteBounded(r) => Ok(AnyTx::SqliteBounded(r.begin(isolation).await?)),
            #[cfg(feature = "registry-postgres")]
            AnyRel::Postgres(r) => Ok(AnyTx::Postgres(r.begin(isolation).await?)),
            #[cfg(feature = "registry-postgres")]
            AnyRel::PostgresBounded(r) => Ok(AnyTx::PostgresBounded(r.begin(isolation).await?)),
            #[cfg(feature = "registry-mysql")]
            AnyRel::Mysql(r) => Ok(AnyTx::Mysql(r.begin(isolation).await?)),
            #[cfg(feature = "registry-mysql")]
            AnyRel::MysqlBounded(r) => Ok(AnyTx::MysqlBounded(r.begin(isolation).await?)),
        }
    }
    fn call_bound(&self) -> Option<Duration> {
        each_rel!(self, r => r.call_bound())
    }
    async fn with_lock_held<'a>(
        &'a self,
        lock_sql: &'a str,
        at_most: Duration,
        during: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    ) -> Result<(), RelError> {
        each_rel!(self, r => r.with_lock_held(lock_sql, at_most, during).await)
    }
}

#[cfg_attr(
    not(any(
        feature = "registry-sqlite",
        feature = "registry-postgres",
        feature = "registry-mysql"
    )),
    allow(
        unused_variables,
        reason = "with no relational driver built in, every arm but `Never` is compiled out"
    )
)]
impl RelationalTx for AnyTx<'_> {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        each_tx!(self, t => t.execute(sql, params).await)
    }
    async fn query(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        each_tx!(self, t => t.query(sql, params).await)
    }
    async fn commit(self) -> Result<(), RelError> {
        each_tx!(self, t => t.commit().await)
    }
    async fn rollback(self) -> Result<(), RelError> {
        each_tx!(self, t => t.rollback().await)
    }
}

/// An opened backend. Use [`Handle::kv`] for backend-agnostic calls.
#[derive(Debug)]
pub struct Handle {
    id: &'static str,
    kv: AnyKv,
    blob: Option<AnyBlob>,
    rel: Option<AnyRel>,
}

/// The key prefixes the key-value store keeps its own files under on the `fs` provider (`kvx/` the
/// current layout, `kv/` a directory written by 0.9.41 or earlier).
const RESERVED: [&str; 2] = ["kv", "kvx"];

/// Whether `key` is one of the reserved names or lies under one.
fn reserved(key: &str) -> Option<&'static str> {
    RESERVED.into_iter().find(|r| {
        key == *r
            || key
                .strip_prefix(r)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

fn refuse(key: &str, prefix: &str) -> BlobError {
    BlobError::Backend(format!(
        "key `{key}` is under `{prefix}/`, which the registry keeps for the key-value store; choose another key"
    ))
}

/// The blob store of whichever provider was opened. It implements [`BlobBackend`], forwarding every
/// method, so a program's calls do not name a backend.
///
/// The keys `kv`, `kvx` and anything under `kv/` or `kvx/` are **reserved** on every provider, not
/// only on `fs` where the key-value store shares the directory: a program must not behave
/// differently by the id in its configuration. A call naming a reserved key returns
/// [`BlobError::Backend`] naming the prefix and touches nothing; a listing leaves reserved entries
/// out and refuses a prefix that lies inside a reserved one.
#[derive(Debug)]
pub struct AnyBlob {
    inner: OpendalBlob,
}

impl AnyBlob {
    /// The facts a matrix row for this backend is recorded under.
    pub fn config(&self) -> &Config {
        self.inner.config()
    }
}

fn check(key: &str) -> Result<(), BlobError> {
    match reserved(key) {
        Some(r) => Err(refuse(key, r)),
        None => Ok(()),
    }
}

impl BlobBackend for AnyBlob {
    fn backend(&self) -> &str {
        self.inner.backend()
    }
    fn version(&self) -> &str {
        self.inner.version()
    }
    fn max_value_size(&self) -> Option<u64> {
        self.inner.max_value_size()
    }
    fn key_layout(&self) -> KeyLayout {
        self.inner.key_layout()
    }
    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, BlobError> {
        check(key)?;
        self.inner.read(key).await
    }
    async fn write(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        check(key)?;
        self.inner.write(key, bytes).await
    }
    async fn delete(&self, key: &str) -> Result<(), BlobError> {
        check(key)?;
        self.inner.delete(key).await
    }
    async fn stat(&self, key: &str) -> Result<Option<Entry>, BlobError> {
        check(key)?;
        self.inner.stat(key).await
    }
    async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Page, BlobError> {
        // A prefix inside a reserved name (`kv/`, `kvx/a`) is refused; a shorter one (``, `k`) lists
        // everything else.
        if let Some(r) = reserved(prefix) {
            return Err(refuse(prefix, r));
        }
        let mut page = self.inner.list(prefix, after, limit).await?;
        page.entries.retain(|e| reserved(&e.key).is_none());
        Ok(page)
    }
    async fn create_exclusive(&self, key: &str, bytes: Vec<u8>) -> Result<bool, BlobError> {
        check(key)?;
        self.inner.create_exclusive(key, bytes).await
    }
    async fn replace_atomic(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        check(key)?;
        self.inner.replace_atomic(key, bytes).await
    }
    async fn append(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        check(key)?;
        self.inner.append(key, bytes).await
    }
    async fn read_range(
        &self,
        key: &str,
        offset: u64,
        len: u64,
    ) -> Result<Option<Vec<u8>>, BlobError> {
        check(key)?;
        self.inner.read_range(key, offset, len).await
    }
    async fn read_range_if(
        &self,
        key: &str,
        offset: u64,
        len: u64,
        tag: &str,
    ) -> Result<Option<Vec<u8>>, BlobError> {
        check(key)?;
        self.inner.read_range_if(key, offset, len, tag).await
    }
    async fn write_stream(&self, key: &str) -> Result<Box<dyn BlobSink>, BlobError> {
        check(key)?;
        self.inner.write_stream(key).await
    }
    async fn write_stream_atomic(&self, key: &str) -> Result<Box<dyn BlobSink>, BlobError> {
        check(key)?;
        self.inner.write_stream_atomic(key).await
    }
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

    /// The blob store: `Some` exactly when the needs named a `blob.*` capability, so a program that
    /// asked for none is not handed one it did not gate on.
    pub fn blob(&self) -> Option<&AnyBlob> {
        self.blob.as_ref()
    }

    /// The relational database: `Some` exactly when the needs named a `rel.*` capability. Statements
    /// are written in the dialect it reports.
    pub fn rel(&self) -> Option<&AnyRel> {
        self.rel.as_ref()
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
    let kinds = kinds_named(needs);
    let p = provider_for(spec, m, options.call_bound.is_some(), &kinds);
    provider::providers(std::slice::from_ref(&p), needs, m).map_err(OpenError::Unmet)?;
    let kv = open_kv(spec.id, endpoint, options.call_bound).await?;
    if kv.backend() != spec.kv_backend {
        return Err(OpenError::ServerMismatch {
            id: spec.id,
            found: kv.backend().to_string(),
        });
    }
    let blob = if kinds.contains(&Kind::Blob) {
        Some(open_blob(spec.id, endpoint)?)
    } else {
        None
    };
    let rel = if kinds.contains(&Kind::Rel) {
        Some(open_rel(spec.id, endpoint, options.call_bound).await?)
    } else {
        None
    };
    // Again, for what was actually opened: one surface per kind the needs name, from the real
    // `backend()` / `version()` / config of that object.
    let kv_surface = Surface::new(
        kv.backend(),
        kv.version(),
        with_bound(
            &surface_for(spec, m, kv.call_bound().is_some()).config,
            kv.call_bound().is_some(),
        ),
    );
    let mut opened: Vec<(Kind, Surface)> = Vec::new();
    if kinds.contains(&Kind::Kv) {
        opened.push((Kind::Kv, kv_surface.clone()));
    }
    if let Some(b) = &blob {
        opened.push((
            Kind::Blob,
            Surface::new(b.backend(), b.version(), b.config().clone()),
        ));
    }
    if let Some(r) = &rel {
        let dialect = r.backend().to_string();
        opened.push((
            Kind::Rel,
            Surface::new(
                r.backend(),
                r.version(),
                Config::new().with("dialect", dialect),
            ),
        ));
    }
    let declared = weakest_declared(&opened);
    let actual = Provider::new(
        spec.id,
        opened.into_iter().map(|(_, surface)| surface).collect(),
        declared,
        p.endpoint().clone(),
    );
    provider::providers(std::slice::from_ref(&actual), needs, m).map_err(OpenError::Unmet)?;
    Ok(Handle {
        id: spec.id,
        kv,
        blob,
        rel,
    })
}

/// Open the blob store of provider `id`: the same directory the key-value store sits on for `fs`, a
/// store of its own for `memory`. The scratch directory is the one the key-value store uses.
fn open_blob(id: &str, endpoint: &Endpoint) -> Result<AnyBlob, OpenError> {
    let open = |e: &dyn fmt::Display| OpenError::Open(e.to_string());
    let inner = match (id, endpoint) {
        ("memory", _) => OpendalBlob::memory().map_err(|e| open(&e))?,
        ("fs", Endpoint::Path(root)) => {
            let mut scratch = root.clone().into_os_string();
            scratch.push(".persistant-atomic");
            OpendalBlob::fs(root, Some(&PathBuf::from(scratch))).map_err(|e| open(&e))?
        }
        _ => return Err(OpenError::Open(format!("no blob opener for `{id}`"))),
    };
    Ok(AnyBlob { inner })
}

/// Open the relational database of provider `id`, plain or through `Bounded`. The tables of the
/// key-value store were created first (by [`open_kv`]), so a bound never covers setup.
async fn open_rel(
    id: &str,
    endpoint: &Endpoint,
    bound: Option<Duration>,
) -> Result<AnyRel, OpenError> {
    #[cfg(any(
        feature = "registry-sqlite",
        feature = "registry-postgres",
        feature = "registry-mysql"
    ))]
    let open = |e: &dyn fmt::Display| OpenError::Open(e.to_string());
    match (id, endpoint) {
        #[cfg(feature = "registry-sqlite")]
        ("sqlite" | "sqlite-full", Endpoint::Path(file)) => {
            let db = if id == "sqlite-full" {
                SqliteBackend::open_durable(file).await
            } else {
                SqliteBackend::open(file).await
            }
            .map_err(|e| open(&e))?;
            Ok(match bound {
                None => AnyRel::Sqlite(db),
                Some(b) => AnyRel::SqliteBounded(Bounded::new(db, b)),
            })
        }
        #[cfg(feature = "registry-postgres")]
        ("postgres", Endpoint::Url(_) | Endpoint::Secret(_)) => {
            let url = endpoint.url()?.unwrap_or_default();
            let db = PgBackend::connect(&url).await.map_err(|e| open(&e))?;
            Ok(match bound {
                None => AnyRel::Postgres(db),
                Some(b) => AnyRel::PostgresBounded(Bounded::new(db, b)),
            })
        }
        #[cfg(feature = "registry-mysql")]
        ("mysql" | "mariadb", Endpoint::Url(_) | Endpoint::Secret(_)) => {
            let url = endpoint.url()?.unwrap_or_default();
            let db = MysqlBackend::connect(&url).await.map_err(|e| open(&e))?;
            Ok(match bound {
                None => AnyRel::Mysql(db),
                Some(b) => AnyRel::MysqlBounded(Bounded::new(db, b)),
            })
        }
        _ => {
            let _ = (endpoint, bound);
            Err(OpenError::Open(format!("no relational opener for `{id}`")))
        }
    }
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
            std::fs::create_dir_all(&scratch).map_err(|e| open(&e))?;
            let kv = KvOnBlob::new(OpendalBlob::fs(root, Some(&scratch)).map_err(|e| open(&e))?);
            // A directory written by 0.9.41 or earlier holds raw `kv/<key>` files: move them to
            // the hex layout (nothing is overwritten; repeating it is harmless).
            kv.migrate_raw_layout().await.map_err(|e| open(&e))?;
            Ok(AnyKv::Blob(kv))
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
        _ => {
            let _ = bound;
            Err(OpenError::Open(format!("no opener for `{id}`")))
        }
    }
}
