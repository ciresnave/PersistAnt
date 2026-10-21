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
//! `registry-sqlite` adds `sqlite` and `sqlite-full`; `all-backends` is every one). A new backend
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

#[cfg(feature = "registry-sqlite")]
use persistant_kv::KvOnRelational;
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
    /// A connection URL.
    Url(String),
}

impl Endpoint {
    /// A filesystem endpoint.
    pub fn path(p: impl AsRef<Path>) -> Self {
        Endpoint::Path(p.as_ref().to_path_buf())
    }

    fn kind(&self) -> &'static str {
        match self {
            Endpoint::None => "none",
            Endpoint::Path(_) => "path",
            Endpoint::Url(_) => "url",
        }
    }

    fn fits(&self, spec: &EndpointSpec) -> bool {
        match (self, spec) {
            (Endpoint::None, EndpointSpec::None) => true,
            (Endpoint::Path(_), EndpointSpec::Path) => true,
            (Endpoint::Url(u), EndpointSpec::Url { schemes }) => u
                .split_once(':')
                .is_some_and(|(s, _)| schemes.iter().any(|x| x == s)),
            _ => false,
        }
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
            OpenError::Open(m) => write!(f, "could not open: {m}"),
        }
    }
}

impl std::error::Error for OpenError {}

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
    #[allow(unused_mut)]
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
    v
}

/// The ids this build offers, in table order. Append-only: see the module docs.
pub fn known_ids() -> Vec<&'static str> {
    specs().iter().map(|s| s.id).collect()
}

fn matrix() -> &'static Matrix {
    static M: OnceLock<Matrix> = OnceLock::new();
    M.get_or_init(|| {
        #[allow(unused_mut)]
        let mut m = Matrix::parse(include_str!("../matrix/opendal.tsv"))
            .expect("embedded matrix/opendal.tsv");
        #[cfg(feature = "registry-sqlite")]
        m.merge(
            Matrix::parse(include_str!("../matrix/sqlx-sqlite.tsv"))
                .expect("embedded matrix/sqlx-sqlite.tsv"),
        );
        m
    })
}

/// The matrix row (version and full config) for a spec, taken from the recorded rows. No row: an
/// empty version, so every lookup is "no row" and the unmet list says so.
fn surface_for(s: &Spec, matrix: &Matrix) -> Surface {
    let row = matrix.rows().iter().find(|r| {
        r.backend == s.kv_backend && s.facts.iter().all(|(k, v)| r.config.get(k) == Some(v))
    });
    match row {
        Some(r) => Surface::new(s.kv_backend, r.version.clone(), r.config.clone()),
        None => {
            let mut c = Config::new();
            for (k, v) in s.facts {
                c = c.with(*k, *v);
            }
            Surface::new(s.kv_backend, "", c)
        }
    }
}

fn provider_for(s: &Spec, matrix: &Matrix) -> Provider {
    Provider::new(
        s.id,
        vec![surface_for(s, matrix)],
        persistant_kv::choose::declared_durability(s.kv_backend),
        (s.endpoint)(),
    )
}

fn table(matrix: &Matrix) -> Vec<Provider> {
    specs().iter().map(|s| provider_for(s, matrix)).collect()
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

fn listing(s: &Sorted) -> Listing {
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
        p50_ns: s.p50_ns,
        notes: spec.notes.to_vec(),
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
    let sorted = provider::select(&table(m), needs, m, speed)?;
    Ok(sorted.iter().map(listing).collect())
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
}

macro_rules! each {
    ($self:ident, $k:ident => $e:expr) => {
        match $self {
            AnyKv::Blob($k) => $e,
            #[cfg(feature = "registry-sqlite")]
            AnyKv::Sqlite($k) => $e,
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
    let m = matrix();
    let all = specs();
    let Some(spec) = all.iter().find(|s| s.id == id) else {
        return Err(OpenError::UnknownBackend {
            id: id.to_string(),
            known: all.iter().map(|s| s.id).collect(),
        });
    };
    let want = (spec.endpoint)();
    if !endpoint.fits(&want) {
        return Err(OpenError::WrongEndpoint {
            id: spec.id,
            wanted: want,
            got: endpoint.kind(),
        });
    }
    let p = provider_for(spec, m);
    provider::providers(std::slice::from_ref(&p), needs, m).map_err(OpenError::Unmet)?;
    let kv = open_kv(spec.id, endpoint).await?;
    // Again, for what was actually opened.
    let actual = Provider::new(
        spec.id,
        vec![Surface::new(
            kv.backend(),
            kv.version(),
            p.primary().config.clone(),
        )],
        persistant_kv::choose::declared_durability(kv.backend()),
        p.endpoint().clone(),
    );
    provider::providers(std::slice::from_ref(&actual), needs, m).map_err(OpenError::Unmet)?;
    Ok(Handle { id: spec.id, kv })
}

async fn open_kv(id: &str, endpoint: &Endpoint) -> Result<AnyKv, OpenError> {
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
            Ok(AnyKv::Blob(KvOnBlob::new(
                OpendalBlob::fs(root, Some(&scratch)).map_err(|e| open(&e))?,
            )))
        }
        #[cfg(feature = "registry-sqlite")]
        ("sqlite", Endpoint::Path(file)) => {
            let db = SqliteBackend::open(file).await.map_err(|e| open(&e))?;
            Ok(AnyKv::Sqlite(
                KvOnRelational::open(db).await.map_err(|e| open(&e))?,
            ))
        }
        #[cfg(feature = "registry-sqlite")]
        ("sqlite-full", Endpoint::Path(file)) => {
            let db = SqliteBackend::open_durable(file)
                .await
                .map_err(|e| open(&e))?;
            Ok(AnyKv::Sqlite(
                KvOnRelational::open(db).await.map_err(|e| open(&e))?,
            ))
        }
        _ => Err(OpenError::Open(format!("no opener for `{id}`"))),
    }
}
