// SPDX-License-Identifier: MIT OR Apache-2.0
//! Reading sqlx's offline query data (`.sqlx/query-<sha256 of the literal>.json`) for the sqlx bridge
//! macros (`docs/DESIGN-SQLX-BRIDGE.md` section 3): the column names, PostgreSQL types and
//! nullability, and the parameter types, as sqlx's own macros recorded them from a real server.
//!
//! This is the only source of result types in the bridge: nothing here is inferred from the SQL
//! text. The type mapping is sqlx's (PostgreSQL, with its `chrono`, `uuid` and `json` features), so a
//! generated record has the field types `sqlx::query!` would have given it.

use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// One result column, as the macro will declare it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Column {
    /// The field name: sqlx's name with its `!`, `?` and `: Type` markers removed.
    pub name: String,
    /// The Rust type, without `Option`.
    pub rust_type: String,
    /// Whether the field is an `Option` of it.
    pub nullable: bool,
}

/// What sqlx recorded about one statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Described {
    pub columns: Vec<Column>,
    /// The PostgreSQL type of each parameter `$1..$n`, in sqlx's spelling (`Uuid`, `Text`, ...).
    pub params: Vec<String>,
}

/// The file name sqlx gives a statement: `query-` and the SHA-256 of its text, in hex.
pub(crate) fn file_name(sql: &str) -> String {
    let digest = Sha256::digest(sql.as_bytes());
    let mut name = String::from("query-");
    for b in digest {
        name.push_str(&format!("{b:02x}"));
    }
    name.push_str(".json");
    name
}

/// The directory sqlx reads offline data from: `SQLX_OFFLINE_DIR` if set, else `.sqlx` in the
/// crate's manifest directory, else `.sqlx` in an ancestor (a workspace root).
pub(crate) fn offline_dir(env_dir: Option<&str>, manifest_dir: &Path) -> Option<PathBuf> {
    if let Some(d) = env_dir {
        return Some(PathBuf::from(d));
    }
    manifest_dir
        .ancestors()
        .map(|a| a.join(".sqlx"))
        .find(|d| d.is_dir())
}

/// The Rust type sqlx gives a PostgreSQL type (its `chrono`, `uuid` and `json` features on); `None`
/// for a type it has no default for, which the caller reports as needing a `: Type` override.
pub(crate) fn rust_type(pg: &str) -> Option<&'static str> {
    Some(match pg {
        "Bool" => "bool",
        "Int2" => "i16",
        "Int4" => "i32",
        "Int8" => "i64",
        "Float4" => "f32",
        "Float8" => "f64",
        "Text" | "Varchar" | "Bpchar" | "Name" | "Citext" => "::std::string::String",
        "Bytea" => "::std::vec::Vec<u8>",
        "Uuid" => "::uuid::Uuid",
        "Timestamptz" => "::chrono::DateTime<::chrono::Utc>",
        "Timestamp" => "::chrono::NaiveDateTime",
        "Date" => "::chrono::NaiveDate",
        "Time" => "::chrono::NaiveTime",
        "Json" | "Jsonb" => "::serde_json::Value",
        "BoolArray" => "::std::vec::Vec<bool>",
        "Int2Array" => "::std::vec::Vec<i16>",
        "Int4Array" => "::std::vec::Vec<i32>",
        "Int8Array" => "::std::vec::Vec<i64>",
        "Float8Array" => "::std::vec::Vec<f64>",
        "TextArray" | "VarcharArray" => "::std::vec::Vec<::std::string::String>",
        "UuidArray" => "::std::vec::Vec<::uuid::Uuid>",
        "Void" => "()",
        _ => return None,
    })
}

/// How sqlx's column name is split: `name`, then optionally `!` (never null) or `?` (may be null),
/// then optionally `: Type` (the Rust type, replacing the default).
fn split_name(raw: &str) -> (String, Option<bool>, Option<String>) {
    let (head, ty) = match raw.split_once(':') {
        Some((h, t)) => (h, Some(t.trim().to_string()).filter(|t| !t.is_empty())),
        None => (raw, None),
    };
    let head = head.trim();
    let (name, null) = match head.strip_suffix('!') {
        Some(n) => (n, Some(false)),
        None => match head.strip_suffix('?') {
            Some(n) => (n, Some(true)),
            None => (head, None),
        },
    };
    (name.trim().to_string(), null, ty)
}

/// Parse the contents of one `.sqlx/query-*.json`. `Err` names what is wrong, and which column.
pub(crate) fn parse(text: &str) -> Result<Described, String> {
    let json: Json = serde_json::from_str(text).map_err(|e| format!("not json: {e}"))?;
    let db = json.get("db_name").and_then(Json::as_str).unwrap_or("");
    if db != "PostgreSQL" {
        return Err(format!(
            "the offline data is for {db:?}, the bridge reads PostgreSQL's"
        ));
    }
    let describe = json.get("describe").ok_or("no `describe` object")?;
    let cols = describe
        .get("columns")
        .and_then(Json::as_array)
        .ok_or("no `describe.columns`")?;
    let nullable = describe
        .get("nullable")
        .and_then(Json::as_array)
        .ok_or("no `describe.nullable`")?;
    if nullable.len() != cols.len() {
        return Err(format!(
            "{} columns but {} nullability entries",
            cols.len(),
            nullable.len()
        ));
    }
    let mut columns = Vec::new();
    for (c, n) in cols.iter().zip(nullable) {
        let raw = c
            .get("name")
            .and_then(Json::as_str)
            .ok_or("a column has no name")?;
        let ty = c
            .get("type_info")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("column {raw:?} has no type_info"))?;
        let (name, forced, over) = split_name(raw);
        let rust = match over {
            Some(t) => t,
            None => rust_type(ty)
                .ok_or_else(|| {
                    format!(
                        "column {name:?} has PostgreSQL type {ty}, which has no default Rust type: write `\"{name}: Type\"` in the statement"
                    )
                })?
                .to_string(),
        };
        // sqlx: a marker wins; else the server's answer; an unknown answer is "may be null".
        let nullable = forced.unwrap_or_else(|| n.as_bool().unwrap_or(true));
        columns.push(Column {
            name,
            rust_type: rust,
            nullable,
        });
    }
    let params = describe
        .get("parameters")
        .and_then(|p| p.get("Left"))
        .and_then(Json::as_array)
        .ok_or("no `describe.parameters.Left` (a statement with an unknown parameter count)")?
        .iter()
        .map(|p| {
            p.as_str()
                .map(str::to_string)
                .ok_or("a parameter type is not a string")
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Described { columns, params })
}

/// Read and parse the offline data for `sql` from `dir`. `Err` says what to run (`cargo sqlx
/// prepare`) when the file is missing, because a changed literal has no file.
pub(crate) fn load(dir: &Path, sql: &str) -> Result<Described, String> {
    let file = dir.join(file_name(sql));
    let text = std::fs::read_to_string(&file).map_err(|e| {
        format!(
            "no offline query data for this statement ({}: {e}); run `cargo sqlx prepare` after changing a statement",
            file.display()
        )
    })?;
    let described = parse(&text).map_err(|m| format!("{}: {m}", file.display()))?;
    // sqlx stores the text it hashed; a file whose text differs is not this statement's.
    let stored = serde_json::from_str::<Json>(&text)
        .ok()
        .and_then(|j| j.get("query").and_then(Json::as_str).map(str::to_string));
    if stored.as_deref() != Some(sql) {
        return Err(format!(
            "{}: the stored query text is not this statement's",
            file.display()
        ));
    }
    Ok(described)
}

/// A struct declaration for the columns: `pub struct <name> { pub <col>: <type>, .. }`, with
/// `Option` for a nullable column. The struct reads a row by position, like `#[derive(FromRow)]`.
pub(crate) fn record_struct(name: &str, d: &Described) -> String {
    let fields: Vec<String> = d
        .columns
        .iter()
        .map(|c| {
            if c.nullable {
                format!("pub {}: ::core::option::Option<{}>,", c.name, c.rust_type)
            } else {
                format!("pub {}: {},", c.name, c.rust_type)
            }
        })
        .collect();
    format!("pub struct {name} {{ {} }}", fields.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real files from Humboldt's `backend/.sqlx` (its `origin/main` f54c05c); structure only.
    const COUNT: &str = r#"{"db_name":"PostgreSQL","query":"SELECT count(*) AS \"c!\" FROM students WHERE id = $1","describe":{"columns":[{"ordinal":0,"name":"c!","type_info":"Int8","origin":"Expression"}],"parameters":{"Left":["Uuid"]},"nullable":[null]},"hash":"00a96d931f8c7f0b41502dce0289981540d6708eea143cf51072e7e1c0eb02e1"}"#;

    #[test]
    fn the_file_name_is_sqlxs_for_a_real_statement() {
        // The hash in COUNT is the one sqlx wrote for this literal.
        assert_eq!(
            file_name(r#"SELECT count(*) AS "c!" FROM students WHERE id = $1"#),
            "query-00a96d931f8c7f0b41502dce0289981540d6708eea143cf51072e7e1c0eb02e1.json"
        );
        // A one-character change is a different file: a stale file is never found.
        assert_ne!(
            file_name(r#"SELECT count(*) AS "c!" FROM students WHERE id = $2"#),
            file_name(r#"SELECT count(*) AS "c!" FROM students WHERE id = $1"#)
        );
    }

    #[test]
    fn a_marked_column_loses_its_marker_and_is_never_null() {
        let d = parse(COUNT).unwrap();
        assert_eq!(d.params, ["Uuid"]);
        assert_eq!(
            d.columns,
            [Column {
                name: "c".into(),
                rust_type: "i64".into(),
                nullable: false
            }]
        );
    }

    fn one(name: &str, ty: &str, nullable: &str) -> String {
        format!(
            r#"{{"db_name":"PostgreSQL","query":"q","describe":{{"columns":[{{"ordinal":0,"name":"{name}","type_info":"{ty}"}}],"parameters":{{"Left":[]}},"nullable":[{nullable}]}}}}"#
        )
    }

    #[test]
    fn nullability_is_sqlxs_marker_then_server_then_assume_null() {
        let col = |n: &str, t: &str, nl: &str| parse(&one(n, t, nl)).unwrap().columns.remove(0);
        assert!(!col("a", "Text", "false").nullable);
        assert!(col("a", "Text", "true").nullable);
        // The server could not say (an expression, an outer join): may be null.
        assert!(col("a", "Text", "null").nullable);
        // A marker wins over the server, both ways.
        assert!(!col("a!", "Text", "true").nullable);
        assert!(col("a?", "Text", "false").nullable);
    }

    #[test]
    fn a_type_override_replaces_the_default_and_unknown_types_say_what_to_write() {
        let c = parse(&one("n!: i16", "Int4", "null"))
            .unwrap()
            .columns
            .remove(0);
        assert_eq!(
            (c.name.as_str(), c.rust_type.as_str(), c.nullable),
            ("n", "i16", false)
        );
        let c = parse(&one("n: MyEnum", "Text", "false"))
            .unwrap()
            .columns
            .remove(0);
        assert_eq!(c.rust_type, "MyEnum");
        let e = parse(&one("p", "Numeric", "false")).unwrap_err();
        assert!(e.contains("`\"p: Type\"`") && e.contains("Numeric"), "{e}");
    }

    #[test]
    fn wrong_databases_and_inconsistent_files_are_errors() {
        assert!(
            parse(&COUNT.replace("PostgreSQL", "MySQL"))
                .unwrap_err()
                .contains("MySQL")
        );
        assert!(parse("{").is_err());
        assert!(parse(&COUNT.replace(r#""nullable":[null]"#, r#""nullable":[]"#)).is_err());
    }

    #[test]
    fn a_record_struct_has_option_for_nullable_columns() {
        let d = Described {
            columns: vec![
                Column {
                    name: "id".into(),
                    rust_type: rust_type("Uuid").unwrap().into(),
                    nullable: false,
                },
                Column {
                    name: "note".into(),
                    rust_type: rust_type("Text").unwrap().into(),
                    nullable: true,
                },
            ],
            params: vec![],
        };
        assert_eq!(
            record_struct("Row", &d),
            "pub struct Row { pub id: ::uuid::Uuid, pub note: ::core::option::Option<::std::string::String>, }"
        );
    }

    #[test]
    fn load_finds_the_file_by_hash_and_refuses_a_missing_or_foreign_one() {
        let dir = tempfile::tempdir().unwrap();
        let sql = r#"SELECT count(*) AS "c!" FROM students WHERE id = $1"#;
        let e = load(dir.path(), sql).unwrap_err();
        assert!(e.contains("cargo sqlx prepare"), "{e}");
        std::fs::write(dir.path().join(file_name(sql)), COUNT).unwrap();
        assert_eq!(load(dir.path(), sql).unwrap().columns[0].name, "c");
        // A file under this statement's name that holds another statement's text.
        let other = "SELECT 1";
        std::fs::write(dir.path().join(file_name(other)), COUNT).unwrap();
        assert!(
            load(dir.path(), other)
                .unwrap_err()
                .contains("not this statement's")
        );
    }

    /// Every file of a real project's `.sqlx` (Humboldt's: 566) parses, and each file name is the hash
    /// of the query text it holds. Runs only when `PERSISTANT_SQLX_SAMPLE_DIR` names such a directory.
    #[test]
    fn every_file_of_a_real_offline_directory_parses() {
        let Ok(dir) = std::env::var("PERSISTANT_SQLX_SAMPLE_DIR") else {
            eprintln!("skipped: PERSISTANT_SQLX_SAMPLE_DIR is not set");
            return;
        };
        let mut seen = 0;
        let mut failures = std::collections::BTreeMap::<String, usize>::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let query = serde_json::from_str::<Json>(&text).unwrap()["query"]
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(
                path.file_name().unwrap().to_string_lossy(),
                file_name(&query)
            );
            if let Err(e) = parse(&text) {
                *failures.entry(e).or_default() += 1;
            }
            seen += 1;
        }
        assert!(seen > 0, "{dir} holds no query files");
        eprintln!("{seen} files, failures: {failures:#?}");
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn the_offline_directory_is_the_variable_then_the_nearest_dot_sqlx() {
        let root = tempfile::tempdir().unwrap();
        let krate = root.path().join("backend");
        std::fs::create_dir_all(krate.join("src")).unwrap();
        assert_eq!(offline_dir(Some("/x"), &krate), Some(PathBuf::from("/x")));
        assert_eq!(offline_dir(None, &krate), None);
        std::fs::create_dir(root.path().join(".sqlx")).unwrap();
        assert_eq!(offline_dir(None, &krate), Some(root.path().join(".sqlx")));
        std::fs::create_dir(krate.join(".sqlx")).unwrap();
        assert_eq!(offline_dir(None, &krate), Some(krate.join(".sqlx")));
    }
}
