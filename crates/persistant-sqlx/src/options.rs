// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{PgBackend, classify};
use persistant_relational::{ConnectionSecret, RelError};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

/// How a connection is secured. A typed setting, not a `sslmode=` string someone has to remember.
///
/// The policy given to [`PgBackend::connect_with`] (or `MysqlBackend::connect_with`) **overrides** whatever `sslmode` the connection
/// string carries, so a URL cannot weaken it. The default is [`TlsPolicy::Require`]: encryption is
/// on unless a program says otherwise in code.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum TlsPolicy {
    /// No TLS. For a loopback or socket connection in a test; never over a network.
    Disable,
    /// TLS if the server offers it, plain otherwise (sqlx's default). **A downgrade is silent**:
    /// an attacker who strips the offer gets a plain connection.
    Prefer,
    /// TLS, or fail. The certificate is **not** checked, so this stops eavesdropping but not an
    /// impostor server.
    #[default]
    Require,
    /// TLS, and the server's certificate must chain to a trusted root (`root_cert`, or the
    /// built-in web roots if `None`). The host name is not checked.
    VerifyCa {
        /// A PEM file of the root(s) to trust, instead of the built-in roots.
        root_cert: Option<PathBuf>,
    },
    /// As `VerifyCa`, and the certificate must also name the host connected to.
    VerifyFull {
        /// A PEM file of the root(s) to trust, instead of the built-in roots.
        root_cert: Option<PathBuf>,
    },
}

/// Settings for [`PgBackend::connect_with`] (and `MysqlBackend::connect_with`, feature `mysql`).
///
/// `#[non_exhaustive]`: build one from [`ConnectOptions::default`] and set what differs, with the
/// `with_*` methods or by assigning a field, so a later setting is not a breaking change. A struct
/// literal from another crate does not compile:
///
/// ```compile_fail
/// use persistant_sqlx::{ConnectOptions, TlsPolicy};
/// let _ = ConnectOptions {
///     tls: TlsPolicy::Require,
///     max_connections: 4,
///     acquire_timeout: std::time::Duration::from_secs(5),
///     application_name: None,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectOptions {
    /// How the connection is secured. Overrides the URL's `sslmode`.
    pub tls: TlsPolicy,
    /// Most connections the pool opens.
    pub max_connections: u32,
    /// How long a caller waits for a connection before [`RelError::Unavailable`]. Bounded by
    /// default (10 s; sqlx's own default is 30 s) so a saturated pool is an error, not a hang.
    /// Configurable: set it here or with [`ConnectOptions::with_acquire_timeout`].
    pub acquire_timeout: Duration,
    /// The name the server shows for this connection (`pg_stat_activity.application_name`, the
    /// `%a` of a log line), so an operator can tell which program holds a connection. `Some` **overrides**
    /// the URL's own `application_name`; `None` leaves the URL's alone. PostgreSQL only: MySQL has no
    /// such setting, and `MysqlBackend::connect_with` ignores it.
    pub application_name: Option<String>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        ConnectOptions {
            tls: TlsPolicy::default(),
            max_connections: 16,
            acquire_timeout: Duration::from_secs(10),
            application_name: None,
        }
    }
}

impl ConnectOptions {
    /// Set how the connection is secured ([`ConnectOptions::tls`]).
    #[must_use]
    pub fn with_tls(mut self, tls: TlsPolicy) -> Self {
        self.tls = tls;
        self
    }

    /// Set the most connections the pool opens ([`ConnectOptions::max_connections`]).
    #[must_use]
    pub fn with_max_connections(mut self, max_connections: u32) -> Self {
        self.max_connections = max_connections;
        self
    }

    /// Set how long a caller waits for a connection ([`ConnectOptions::acquire_timeout`]; the default is
    /// 10 s, sqlx's own is 30 s).
    #[must_use]
    pub fn with_acquire_timeout(mut self, acquire_timeout: Duration) -> Self {
        self.acquire_timeout = acquire_timeout;
        self
    }

    /// Set the name the server shows for this connection ([`ConnectOptions::application_name`]).
    #[must_use]
    pub fn with_application_name(mut self, name: impl Into<String>) -> Self {
        self.application_name = Some(name.into());
        self
    }
}

/// The sqlx connect options for `secret` under `options`' TLS policy and application name: shared by
/// the pool ([`PgBackend::connect_with`]) and the single connection (`native::PgSession`).
pub(crate) fn pg_connect_options(
    secret: &ConnectionSecret,
    options: &ConnectOptions,
) -> Result<PgConnectOptions, RelError> {
    {
        let url = secret.resolve()?;
        let mut o = PgConnectOptions::from_str(url.expose())
            .map_err(|_| RelError::Backend("connection string is not valid".to_string()))?;
        o = match &options.tls {
            TlsPolicy::Disable => o.ssl_mode(PgSslMode::Disable),
            TlsPolicy::Prefer => o.ssl_mode(PgSslMode::Prefer),
            TlsPolicy::Require => o.ssl_mode(PgSslMode::Require),
            TlsPolicy::VerifyCa { root_cert } => {
                let o = o.ssl_mode(PgSslMode::VerifyCa);
                match root_cert {
                    Some(p) => o.ssl_root_cert(p),
                    None => o,
                }
            }
            TlsPolicy::VerifyFull { root_cert } => {
                let o = o.ssl_mode(PgSslMode::VerifyFull);
                match root_cert {
                    Some(p) => o.ssl_root_cert(p),
                    None => o,
                }
            }
        };
        if let Some(name) = &options.application_name {
            o = o.application_name(name);
        }
        Ok(o)
    }
}

impl PgBackend {
    /// Connect with an explicit TLS policy and pool bounds, to a connection string held as a
    /// [`ConnectionSecret`] (resolved here, never printed).
    pub async fn connect_with(
        secret: &ConnectionSecret,
        options: &ConnectOptions,
    ) -> Result<Self, RelError> {
        let o = pg_connect_options(secret, options)?;
        let pool = PgPoolOptions::new()
            .max_connections(options.max_connections)
            .acquire_timeout(options.acquire_timeout)
            .connect_with(o)
            .await
            .map_err(classify)?;
        Self::from_pool(pool).await
    }

    /// Whether the connection this backend uses is encrypted, as the **server** reports it
    /// (`pg_stat_ssl`), not as the client was asked.
    pub async fn connection_is_encrypted(&self) -> Result<bool, RelError> {
        sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .fetch_one(&self.pool)
            .await
            .map_err(classify)
    }
}
