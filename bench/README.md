# bench: speed data (benchmarking stage 3)

Each `.tsv` is a `persistant_kv::speed::SpeedTable` (`sqlx-rel-speed.tsv` is the relational kind's,
`persistant_relational::speed`; both kinds share the core types, so `speed_report` reads either): one row per workload, backend and
environment. A row's `env` column says where and when it was measured (CPU model and cores, OS,
architecture, date, and the server image digest when a server ran in a container); its `rerun`
column is the command that reproduces it.

**These numbers are not a property of the backends.** They hold on the machine in `env`, on that
date, for that backend version. A different machine can reverse an order. `speed_report` therefore
prints each ordering under its environment and never merges two. A backend that fails the contract
for a workload (`status = excluded`) has no number and is not ranked.

Read them with:

    cargo run -q -p persistant-kv --example speed_report -- bench/opendal-speed.tsv bench/sqlx-speed.tsv bench/sqlx-rel-speed.tsv

The checked-in files are one dated run from a developer machine (Windows; PostgreSQL 16 and MySQL 8.4
in Docker Desktop containers, so their figures include the VM's network). CI produces its own on
its runner and uploads them as artifacts. Nothing compares a figure to a constant.
