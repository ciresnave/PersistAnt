-- SPDX-License-Identifier: MIT OR Apache-2.0
-- The schema the `persistant::sql!` doc-tests check against (src/lib.rs).
CREATE TABLE users (id BIGINT NOT NULL PRIMARY KEY, email TEXT NOT NULL, note TEXT);
