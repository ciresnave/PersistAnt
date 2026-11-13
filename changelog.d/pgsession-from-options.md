kind: `PgSession::connect_with_options`
---
`native::PgSession::connect_with_options(PgConnectOptions, SessionSettings)` connects from ready-made sqlx options, for a caller that holds options rather than a URL (the options of a test pool): options cannot be turned back into a URL without losing the password and TLS settings. `PgSession::connect` now builds its options and calls it, so the two share one path. Additive. Verified against PostgreSQL 16: the session carries the settings, and an error from a wrong password does not contain it.
