kind: feat: `sqlx_query!` binds `Vec<Uuid>`, `Vec<bool>`, `Vec<i64>`, `Vec<f64>` and `Vec<String>` array parameters
---
A parameter sqlx describes as `UuidArray`, `BoolArray`, `Int8Array`, `Float8Array`, `TextArray` or `VarcharArray` now carries its element type from the macro to the bind (`Bound::with_array_hints`), so `WHERE id = ANY($1)` with a `Vec<Uuid>` binds as `uuid[]`, and an empty array binds with its described type instead of having none to infer. A statement with no array parameter expands exactly as before. Verified on `OnConn` against PostgreSQL 16 by `tests/sqlx_bridge_array.rs` (values and empty), which fails with `operator does not exist: uuid = text` when the hint is not emitted; the macro's own unit tests cover the hint list and that an array placeholder gets no cast. An array parameter is accepted only in the `= ANY($n)` form (any other use is `Unsupported`, as for the other backends).

— lane: persistant
