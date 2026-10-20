// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed rows: `#[derive(FromRow)]`, and the reading of a `SELECT` list for `sql!(as T, ..)`.
//!
//! The derive sees the struct and nothing else; the macro sees the statement and the schema and nothing
//! else. The two meet at compile time through constants: the derive states the fields as
//! `FromRow::FIELDS`, the macro emits `const` assertions comparing the `SELECT` list with them
//! (`persistant_relational::row::check`), so a mismatch is a build error naming the column.

use proc_macro2::{Delimiter, Group, Span, TokenStream, TokenTree};
use unisql::ast::{self, Expr, SelectItem, SetExpr, TableRef};
use unisql::{ColumnClass, Schema};

/// The path the derive names the relational crate through, unless `#[persistant(crate = path)]` says.
pub(crate) const DEFAULT_RELATIONAL: &str = "::persistant::relational";

pub(crate) struct DeriveError {
    pub span: Span,
    pub message: String,
}

fn derr(span: Span, message: impl Into<String>) -> DeriveError {
    DeriveError {
        span,
        message: message.into(),
    }
}

/// `#[derive(FromRow)]` on a struct with named fields, each of a type that is `FromValue`.
pub(crate) fn derive_from_row(input: TokenStream) -> Result<String, DeriveError> {
    let mut toks = input.into_iter().peekable();
    let mut path = DEFAULT_RELATIONAL.to_string();

    // Attributes, visibility, `struct Name`.
    loop {
        match toks.peek() {
            Some(TokenTree::Punct(p)) if p.as_char() == '#' => {
                toks.next();
                if let Some(TokenTree::Group(g)) = toks.next()
                    && let Some(custom) = persistant_crate(&g)
                {
                    path = custom;
                }
            }
            Some(TokenTree::Ident(i)) if i == "pub" => {
                toks.next();
                if matches!(toks.peek(), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis)
                {
                    toks.next();
                }
            }
            _ => break,
        }
    }
    match toks.next() {
        Some(TokenTree::Ident(i)) if i == "struct" => {}
        Some(other) => {
            return Err(derr(
                other.span(),
                "FromRow can only be derived for a struct with named fields",
            ));
        }
        None => return Err(derr(Span::call_site(), "empty input")),
    }
    let Some(TokenTree::Ident(name)) = toks.next() else {
        return Err(derr(Span::call_site(), "expected the struct's name"));
    };
    let body = match toks.next() {
        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => g,
        Some(TokenTree::Punct(p)) if p.as_char() == '<' => {
            return Err(derr(
                p.span(),
                "FromRow cannot be derived for a generic struct",
            ));
        }
        Some(other) => {
            return Err(derr(
                other.span(),
                "FromRow needs a struct with named fields (a tuple or unit struct has no column names to check)",
            ));
        }
        None => return Err(derr(name.span(), "expected the struct's fields")),
    };

    // Fields: [attributes] [pub[(..)]] name : Type , ...
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut cur: Vec<TokenTree> = Vec::new();
    let mut depth = 0_i32;
    let mut chunks: Vec<Vec<TokenTree>> = Vec::new();
    for t in body.stream() {
        match &t {
            TokenTree::Punct(p) if p.as_char() == '<' => depth += 1,
            TokenTree::Punct(p) if p.as_char() == '>' => depth -= 1,
            TokenTree::Punct(p) if p.as_char() == ',' && depth == 0 => {
                chunks.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(t);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    for chunk in chunks {
        let mut it = chunk.into_iter().peekable();
        loop {
            match it.peek() {
                Some(TokenTree::Punct(p)) if p.as_char() == '#' => {
                    it.next();
                    it.next();
                }
                Some(TokenTree::Ident(i)) if i == "pub" => {
                    it.next();
                    if matches!(it.peek(), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis)
                    {
                        it.next();
                    }
                }
                _ => break,
            }
        }
        let Some(TokenTree::Ident(field)) = it.next() else {
            return Err(derr(Span::call_site(), "expected a field name"));
        };
        match it.next() {
            Some(TokenTree::Punct(p)) if p.as_char() == ':' => {}
            _ => return Err(derr(field.span(), "expected `:` after the field name")),
        }
        let ty: TokenStream = it.collect();
        if ty.is_empty() {
            return Err(derr(field.span(), "the field has no type"));
        }
        fields.push((
            field.to_string().trim_start_matches("r#").to_string(),
            ty.to_string(),
        ));
    }
    if fields.is_empty() {
        return Err(derr(
            name.span(),
            "a struct with no fields reads no columns",
        ));
    }

    let describe: Vec<String> = fields
        .iter()
        .map(|(f, ty)| {
            format!(
                "{path}::Field {{ name: {f:?}, kind: <{ty} as {path}::FromValue>::KIND, \
                 nullable: <{ty} as {path}::FromValue>::NULLABLE }}"
            )
        })
        .collect();
    let reads: Vec<String> = fields
        .iter()
        .enumerate()
        .map(|(i, (f, ty))| {
            format!(
                "{f}: {path}::row::read::<{ty}>(row, {i}, {f:?})?",
                f = f,
                ty = ty,
                i = i,
                path = path
            )
        })
        .collect();
    Ok(format!(
        "impl {path}::FromRow for {name} {{ \
         const FIELDS: &'static [{path}::Field] = &[{describe}]; \
         fn from_row(row: &{path}::Row) -> ::core::result::Result<Self, {path}::RelError> {{ \
         ::core::result::Result::Ok({name} {{ {reads} }}) }} }}",
        describe = describe.join(", "),
        reads = reads.join(", ")
    ))
}

/// `persistant(crate = some::path)` inside `#[..]`.
fn persistant_crate(g: &Group) -> Option<String> {
    let mut it = g.stream().into_iter();
    match it.next() {
        Some(TokenTree::Ident(i)) if i == "persistant" => {}
        _ => return None,
    }
    let Some(TokenTree::Group(inner)) = it.next() else {
        return None;
    };
    let mut inner = inner.stream().into_iter();
    match (inner.next(), inner.next()) {
        (Some(TokenTree::Ident(k)), Some(TokenTree::Punct(eq)))
            if k == "crate" && eq.as_char() == '=' =>
        {
            let path: TokenStream = inner.collect();
            Some(path.to_string().replace(' ', ""))
        }
        _ => None,
    }
}

// ---- the SELECT list -----------------------------------------------------------------------

/// What `sql!(as T, ..)` expects of one result column.
pub(crate) struct Col {
    pub name: String,
    pub class: ColumnClass,
    /// `Some(true)`: the column can be NULL, so the field must be an `Option`. `None`: not known.
    pub nullable: Option<bool>,
}

/// The type of an annotated expression column (`AS "n: i64"`), as a class and nullability.
fn annotation(ty: &str) -> Option<(ColumnClass, bool)> {
    let ty = ty.trim();
    if let Some(inner) = ty.strip_prefix("Option<").and_then(|t| t.strip_suffix('>')) {
        return annotation(inner).map(|(c, _)| (c, true));
    }
    let class = match ty.replace(' ', "").as_str() {
        "i64" | "i32" | "f64" => ColumnClass::Numeric,
        "bool" => ColumnClass::Boolean,
        "String" => ColumnClass::Text,
        "Vec<u8>" => ColumnClass::Other,
        _ => return None,
    };
    Some((class, false))
}

/// The columns a statement returns, with what the schema says of each. Errors are about the statement
/// (it is not a plain `SELECT`, a column is an expression with no annotation, a column does not exist in
/// a table whose column list is complete); anything the schema does not state is unknown.
pub(crate) fn columns(sql: &str, source: &str, schema: &Schema) -> Result<Vec<Col>, String> {
    let stmt = unisql::read_statement(sql, source).map_err(|e| e.to_string())?;
    let ast::Statement::Query(q) = stmt else {
        return Err("`as T` needs a SELECT statement".to_string());
    };
    let SetExpr::Select(sel) = q.body else {
        return Err("`as T` needs a plain SELECT, not UNION, INTERSECT or EXCEPT".to_string());
    };
    // One base table is resolved against the schema; with joins or subqueries the columns are unknown
    // (a join can make a NOT NULL column NULL), which is "no check", never a wrong one.
    let single: Option<(&ast::Name, Option<&str>)> = match &sel.from[..] {
        [TableRef::Table { name, alias }] => Some((name, alias.as_deref())),
        _ => None,
    };
    let mut out = Vec::new();
    for (n, item) in sel.projection.iter().enumerate() {
        let pos = n + 1;
        let SelectItem::Expr { expr, alias } = item else {
            return Err(
                "SELECT * (or t.*) cannot be matched to a struct: list the columns".to_string(),
            );
        };
        if let Some(alias) = alias
            && let Some((name, ty)) = alias.split_once(':')
        {
            let (class, nullable) = annotation(ty).ok_or_else(|| {
                format!(
                    "column {pos}: unknown type `{}` in its annotation; use i64, i32, f64, bool, String, Vec<u8> or Option<..>",
                    ty.trim()
                )
            })?;
            let name = name.trim();
            if name.is_empty() {
                return Err(format!(
                    "column {pos}: the annotation `{alias}` has no name before the `:`"
                ));
            }
            out.push(Col {
                name: name.to_string(),
                class,
                nullable: Some(nullable),
            });
            continue;
        }
        let Expr::Column(col) = expr else {
            return Err(format!(
                "column {pos} is an expression, so its type cannot be known: name and type it, for example `<expr> AS \"{}: i64\"`",
                alias.as_deref().unwrap_or("name")
            ));
        };
        let column = col.last();
        let name = alias.clone().unwrap_or_else(|| column.to_string());
        let (class, nullable) = match single {
            Some((table, table_alias)) => {
                let qualifier = col.0.len().checked_sub(2).map(|i| col.0[i].as_str());
                let ours = qualifier.is_none_or(|q| Some(q) == table_alias || q == table.last());
                if !ours {
                    (ColumnClass::Unknown, None)
                } else if let Some(c) = schema.get(table, column) {
                    (c.class, c.nullable)
                } else if schema.complete_columns(table).is_some() {
                    return Err(format!(
                        "column {pos}: table `{}` has no column `{column}`",
                        table.last()
                    ));
                } else {
                    (ColumnClass::Unknown, None)
                }
            }
            None => (ColumnClass::Unknown, None),
        };
        out.push(Col {
            name,
            class,
            nullable,
        });
    }
    if out.is_empty() {
        return Err("the SELECT returns no columns".to_string());
    }
    Ok(out)
}

/// The `check` mask constant and a word for a class.
pub(crate) fn class_parts(c: ColumnClass) -> (&'static str, &'static str) {
    match c {
        ColumnClass::Numeric => ("NUMERIC", "numeric"),
        ColumnClass::Text => ("TEXT", "text"),
        ColumnClass::Boolean => ("BOOLEAN", "boolean"),
        ColumnClass::Other => ("OTHER", "of another type (bytes, a date, a UUID, JSON)"),
        _ => ("ANY", "of a type the schema does not classify"),
    }
}

/// The `const _: () = { assert!(..); .. };` block that compares the columns with `T::FIELDS`.
pub(crate) fn assertions(rel: &str, ty: &str, cols: &[Col]) -> String {
    let mut s = String::from("const _: () = { ");
    s.push_str(&format!(
        "::core::assert!({rel}::row::check::fields_are::<{ty}>({n}), {msg:?}); ",
        n = cols.len(),
        msg = format!(
            "the SELECT returns {} column(s) but the struct has a different number of fields",
            cols.len()
        )
    ));
    for (i, c) in cols.iter().enumerate() {
        let pos = i + 1;
        let (mask, word) = class_parts(c.class);
        s.push_str(&format!(
            "::core::assert!({rel}::row::check::named::<{ty}>({i}, {name:?}), {m1:?}); \
             ::core::assert!({rel}::row::check::kind_in::<{ty}>({i}, {rel}::row::check::{mask}), {m2:?}); \
             ::core::assert!({rel}::row::check::null_fits::<{ty}>({i}, {nullable}), {m3:?}); ",
            name = c.name,
            nullable = c.nullable == Some(true),
            m1 = format!(
                "column {pos} (`{}`): field {pos} of the struct has a different name",
                c.name
            ),
            m2 = format!(
                "column {pos} (`{}`) is {word}: the type of field {pos} cannot hold it",
                c.name
            ),
            m3 = format!(
                "column {pos} (`{}`) can be NULL: make field {pos} an Option<..>",
                c.name
            ),
        ));
    }
    s.push_str("};");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn derive(src: &str) -> Result<String, String> {
        derive_from_row(src.parse().unwrap()).map_err(|e| e.message)
    }

    const DDL: &str = "CREATE TABLE users (id BIGINT NOT NULL PRIMARY KEY, email TEXT NOT NULL, note TEXT, active BOOLEAN NOT NULL);";

    fn schema() -> Schema {
        unisql::schema_from_ddl(DDL, "postgres").unwrap().schema
    }

    fn cols(sql: &str) -> Result<Vec<(String, ColumnClass, Option<bool>)>, String> {
        columns(sql, "postgres", &schema()).map(|c| {
            c.into_iter()
                .map(|c| (c.name, c.class, c.nullable))
                .collect()
        })
    }

    #[test]
    fn the_derive_states_the_fields_and_reads_them_by_position() {
        let out = derive("#[derive(Debug)] pub struct User { pub id: i64, email: String, note: Option<String>, tags: Vec<u8> }").unwrap();
        assert!(
            out.starts_with("impl ::persistant::relational::FromRow for User"),
            "{out}"
        );
        for want in [
            r#"name: "id""#,
            r#"name: "note""#,
            "<Option<String> as ::persistant::relational::FromValue>::NULLABLE",
            r#"::persistant::relational::row::read::<i64>(row, 0, "id")?"#,
            r#"::persistant::relational::row::read::<Vec<u8>>(row, 3, "tags")?"#,
        ] {
            // `TokenStream::to_string` spaces tokens (`Option < String >`): compare without spaces.
            assert!(
                out.replace(' ', "").contains(&want.replace(' ', "")),
                "missing {want:?} in {out}"
            );
        }
        // The crate path can be named.
        let out =
            derive("#[persistant(crate = ::persistant_relational)] struct A { x: i64 }").unwrap();
        assert!(
            out.contains("::persistant_relational::FromRow for A"),
            "{out}"
        );
        assert!(!out.contains("::persistant::"), "{out}");
    }

    #[test]
    fn what_the_derive_cannot_do_is_named() {
        assert!(
            derive("struct T(i64, String);")
                .unwrap_err()
                .contains("named fields")
        );
        assert!(derive("struct U;").unwrap_err().contains("named fields"));
        assert!(
            derive("struct G<T> { x: T }")
                .unwrap_err()
                .contains("generic")
        );
        assert!(derive("struct E {}").unwrap_err().contains("no fields"));
        assert!(derive("enum X { A }").unwrap_err().contains("struct"));
        assert!(derive("struct P { x i64 }").unwrap_err().contains("`:`"));
        // The control: the same shape with the fault removed derives.
        assert!(derive("struct Ok { x: i64 }").is_ok());
    }

    #[test]
    fn the_select_list_is_read_against_the_schema() {
        use ColumnClass::*;
        assert_eq!(
            cols("SELECT id, email, note, active FROM users").unwrap(),
            [
                ("id".into(), Numeric, Some(false)),
                ("email".into(), Text, Some(false)),
                ("note".into(), Text, Some(true)),
                ("active".into(), Boolean, Some(false)),
            ]
        );
        // An alias renames, a qualifier is resolved, a table alias too.
        assert_eq!(
            cols("SELECT u.email AS mail FROM users AS u WHERE u.id = $1").unwrap(),
            [("mail".into(), Text, Some(false))]
        );
        // An annotated expression carries its own type.
        assert_eq!(
            cols(r#"SELECT count(*) AS "n: i64", max(id) AS "top: Option<i64>" FROM users"#)
                .unwrap(),
            [
                ("n".into(), Numeric, Some(false)),
                ("top".into(), Numeric, Some(true))
            ]
        );
        // A join or a column of another table makes a column unknown, never wrong.
        let j = cols("SELECT u.id FROM users u JOIN users v ON v.id = u.id").unwrap();
        assert_eq!(j, [("id".into(), Unknown, None)]);
    }

    #[test]
    fn a_select_list_that_cannot_be_typed_is_an_error() {
        assert!(
            cols("SELECT * FROM users")
                .unwrap_err()
                .contains("list the columns")
        );
        assert!(
            cols("SELECT u.* FROM users u")
                .unwrap_err()
                .contains("list the columns")
        );
        assert!(
            cols("SELECT id + 1 FROM users")
                .unwrap_err()
                .contains("column 1 is an expression")
        );
        assert!(
            cols(r#"SELECT id AS "n: u128" FROM users"#)
                .unwrap_err()
                .contains("unknown type `u128`")
        );
        assert!(
            cols("SELECT nope FROM users")
                .unwrap_err()
                .contains("has no column `nope`")
        );
        assert!(
            cols("DELETE FROM users")
                .unwrap_err()
                .contains("needs a SELECT")
        );
        assert!(
            cols("SELECT id FROM users UNION SELECT id FROM users")
                .unwrap_err()
                .contains("plain SELECT")
        );
        // The control: a column of a table the schema does not state is unknown, not an error.
        assert_eq!(
            cols("SELECT x FROM elsewhere").unwrap(),
            [("x".into(), ColumnClass::Unknown, None)]
        );
    }

    #[test]
    fn the_assertions_name_the_column_and_what_is_wrong() {
        let c = columns("SELECT id, note FROM users", "postgres", &schema()).unwrap();
        let a = assertions("::r", "User", &c);
        for want in [
            "fields_are::<User>(2)",
            r#"named::<User>(0, "id")"#,
            "kind_in::<User>(0, ::r::row::check::NUMERIC)",
            "null_fits::<User>(1, true)",
            "column 2 (`note`) can be NULL: make field 2 an Option<..>",
            "column 1 (`id`) is numeric: the type of field 1 cannot hold it",
        ] {
            assert!(a.contains(want), "missing {want:?} in {a}");
        }
    }
}
