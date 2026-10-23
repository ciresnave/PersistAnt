// SPDX-License-Identifier: MIT OR Apache-2.0
//! `sqlx_query!`, `sqlx_query_as!` and `sqlx_query_scalar!` (`docs/DESIGN-SQLX-BRIDGE.md` sections 2
//! to 4): the argument shape of sqlx's own macros, expanding to
//!
//! 1. sqlx's own check of the literal, in a closure that is never called (feature `sqlx-check`),
//! 2. the statement translated by UniSQL for the targets (default: PostgreSQL only), with each
//!    placeholder whose type needs it cast from sqlx's recorded parameter types,
//! 3. a `Bound` that a `Runner` (a PersistAnt backend or transaction) runs.
//!
//! Result types come from sqlx's offline data only (`sqlx_offline`), never from a second schema.

use crate::imp::{Error, compile_errors, err, name_list, split_commas, string_value};
use crate::sqlx_offline::{self, Described};
use proc_macro2::{Delimiter, Group, Literal, Span, TokenStream, TokenTree};
use std::path::{Path, PathBuf};

/// Which of the three macros.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Form {
    /// `sqlx_query!`: a record struct generated from the described columns.
    Record,
    /// `sqlx_query_as!`: the caller's `FromRow` struct.
    As,
    /// `sqlx_query_scalar!`: the one column.
    Scalar,
}

impl Form {
    #[cfg_attr(
        not(feature = "sqlx-check"),
        allow(
            dead_code,
            reason = "only the expansion built with feature sqlx-check names the sqlx macro"
        )
    )]
    fn sqlx_macro(self) -> &'static str {
        match self {
            Form::Record => "query",
            Form::As => "query_as",
            Form::Scalar => "query_scalar",
        }
    }
}

/// The macro entry: the one environment read (the crate being compiled, and sqlx's own offline
/// directory variable), then [`expand_in`].
pub(crate) fn expand(form: Form, input: TokenStream) -> TokenStream {
    let dir = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
    let env = std::env::var("SQLX_OFFLINE_DIR").ok();
    expand_in(form, input, dir.as_deref(), env.as_deref())
}

/// As [`expand`], with the environment given (so tests need not touch it).
pub(crate) fn expand_in(
    form: Form,
    input: TokenStream,
    manifest_dir: Option<&Path>,
    offline_env: Option<&str>,
) -> TokenStream {
    match build(form, input, manifest_dir, offline_env) {
        Ok(t) => t,
        Err(e) => compile_errors(e),
    }
}

/// What the call said.
struct Call {
    /// `crate = ::path`: the relational crate the expansion names (default `::persistant::relational`).
    krate: String,
    /// `sqlx = ::path`: the crate holding sqlx's macros (default `::sqlx`; `sqlx = none` skips the check). `targets = [..]` and
    /// `allow = [..]` are as for `sql!`.
    #[cfg_attr(
        not(feature = "sqlx-check"),
        allow(
            dead_code,
            reason = "only the expansion built with feature sqlx-check reads the sqlx path"
        )
    )]
    sqlx: Option<String>,
    targets: Vec<String>,
    allow: Vec<String>,
    ty: Option<(TokenStream, String)>,
    lit: Literal,
    sql: String,
    args: Vec<TokenStream>,
}

fn tokens(s: &str) -> TokenStream {
    s.parse().expect("the macro builds valid tokens")
}

fn path_text(seg: &[TokenTree]) -> String {
    seg.iter().cloned().collect::<TokenStream>().to_string()
}

fn parse(form: Form, input: TokenStream) -> Result<Call, Vec<Error>> {
    let mut segs = split_commas(input.into_iter().collect());
    if segs.last().is_some_and(Vec::is_empty) {
        segs.pop();
    }
    let mut segs = segs.into_iter().peekable();
    let (mut krate, mut sqlx, mut targets, mut allow) = (None, None, None, None);
    // Named options come first: `crate = ..`, `sqlx = ..`, `targets = [..]`.
    while let Some(seg) = segs.peek() {
        let (Some(TokenTree::Ident(name)), Some(TokenTree::Punct(eq))) = (seg.first(), seg.get(1))
        else {
            break;
        };
        if eq.as_char() != '=' {
            break;
        }
        let name = name.to_string();
        if !matches!(name.as_str(), "crate" | "sqlx" | "targets" | "allow") {
            break;
        }
        let seg = segs.next().expect("peeked");
        let value = &seg[2..];
        match name.as_str() {
            "crate" => krate = Some(path_text(value)),
            "sqlx" => sqlx = Some(path_text(value)),
            "targets" => targets = Some(name_list(value).map_err(|e| vec![e])?),
            _ => allow = Some(name_list(value).map_err(|e| vec![e])?),
        }
    }
    let ty = if form == Form::As {
        let Some(seg) = segs.next().filter(|s| !s.is_empty()) else {
            return Err(vec![err(
                Span::call_site(),
                "sqlx_query_as! needs the struct first: sqlx_query_as!(Type, \"SELECT ..\", args..)",
            )]);
        };
        let stream: TokenStream = seg.iter().cloned().collect();
        let text = stream.to_string();
        Some((stream, text))
    } else {
        None
    };
    let lit_seg = segs.next().unwrap_or_default();
    let (lit, sql) = match lit_seg.as_slice() {
        [TokenTree::Literal(l)] => match string_value(l) {
            Some(s) => (l.clone(), s),
            None => {
                return Err(vec![err(
                    l.span(),
                    "the statement must be a string literal",
                )]);
            }
        },
        [first, ..] => {
            return Err(vec![err(
                first.span(),
                "the statement must be a string literal (it is the text sqlx checked)",
            )]);
        }
        [] => return Err(vec![err(Span::call_site(), "no statement given")]),
    };
    let mut args = Vec::new();
    for seg in segs {
        if seg.is_empty() {
            return Err(vec![err(
                Span::call_site(),
                "an empty argument (two commas in a row)",
            )]);
        }
        args.push(seg.into_iter().collect());
    }
    Ok(Call {
        krate: krate.unwrap_or_else(|| "::persistant::relational".into()),
        // `sqlx = none` skips the check for this call (a crate without sqlx's macros).
        sqlx: match sqlx.as_deref() {
            Some("none") => None,
            Some(path) => Some(path.to_string()),
            None => Some("::sqlx".into()),
        },
        targets: targets.unwrap_or_else(|| vec!["postgres".into()]),
        allow: allow.unwrap_or_default(),
        ty,
        lit,
        sql,
        args,
    })
}

/// The cast a parameter of this sqlx type needs, because `Value` carries it as text and
/// PostgreSQL will not compare `uuid = text`.
fn cast_of(sqlx_type: &str) -> Option<&'static str> {
    Some(match sqlx_type {
        "Uuid" => "uuid",
        "Timestamptz" => "timestamptz",
        "Timestamp" => "timestamp",
        "Date" => "date",
        "Time" => "time",
        "Jsonb" => "jsonb",
        "Json" => "json",
        _ => return None,
    })
}

/// `sql` with `$n` replaced by `CAST($n AS type)` where parameter `n` needs it (sections 4 of the
/// design). String literals, quoted identifiers and comments are left alone.
pub(crate) fn rewrite_params(sql: &str, types: &[String]) -> String {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len() + 16);
    let (mut i, mut start) = (0, 0);
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() {
                    if b[i] == q {
                        if b.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b'$' if b.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                let n: usize = sql[i + 1..j].parse().unwrap_or(0);
                if let Some(t) = n
                    .checked_sub(1)
                    .and_then(|k| types.get(k))
                    .and_then(|t| cast_of(t))
                {
                    out.push_str(&sql[start..i]);
                    out.push_str(&format!("CAST({} AS {t})", &sql[i..j]));
                    start = j;
                }
                i = j;
            }
            _ => i += 1,
        }
    }
    out.push_str(&sql[start..]);
    out
}

fn paren(ts: TokenStream) -> TokenTree {
    TokenTree::Group(Group::new(Delimiter::Parenthesis, ts))
}

/// `items` separated by commas.
fn comma_list(items: impl IntoIterator<Item = TokenStream>) -> TokenStream {
    let mut out = TokenStream::new();
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.extend(tokens(","));
        }
        out.extend(item);
    }
    out
}

/// The text of the sql! call that translates `rewritten`, as tokens.
fn statement(call: &Call, rewritten: &str, manifest_dir: Option<&Path>) -> TokenStream {
    let input = tokens(&format!(
        "source = postgres, targets = [{}], allow = [{}], crate = {}::embed, schema = none, {:?}",
        call.targets.join(", "),
        call.allow.join(", "),
        call.krate,
        rewritten
    ));
    crate::imp::sql_in(input, manifest_dir)
}

/// The size check of a `sqlx_query_as!` struct against the described columns.
fn assertions(rel: &str, ty: &str, d: &Described) -> String {
    let mut s = String::from("const _: () = { ");
    s.push_str(&format!(
        "::core::assert!({rel}::row::check::fields_are::<{ty}>({n}), {msg:?}); ",
        n = d.columns.len(),
        msg = format!(
            "sqlx describes {} column(s) but the struct has a different number of fields",
            d.columns.len()
        )
    ));
    for (i, c) in d.columns.iter().enumerate() {
        let pos = i + 1;
        s.push_str(&format!(
            "::core::assert!({rel}::row::check::named::<{ty}>({i}, {name:?}), {m1:?}); \
             ::core::assert!({rel}::row::check::null_fits::<{ty}>({i}, {nullable}), {m2:?}); ",
            name = c.name,
            nullable = c.nullable,
            m1 = format!(
                "column {pos} (`{}`): field {pos} of the struct has a different name",
                c.name
            ),
            m2 = format!(
                "column {pos} (`{}`) can be NULL: make field {pos} an Option<..>",
                c.name
            ),
        ));
    }
    s.push_str("};");
    s
}

fn build(
    form: Form,
    input: TokenStream,
    manifest_dir: Option<&Path>,
    offline_env: Option<&str>,
) -> Result<TokenStream, Vec<Error>> {
    let call = parse(form, input)?;
    // A statement we cannot read the description of still gets sqlx's own check, so its (usually
    // more telling) error comes with ours.
    assemble(form, &call, manifest_dir, offline_env).or_else(|errors| {
        let check = check_tokens(form, &call);
        if check.is_empty() {
            return Err(errors);
        }
        let mut both = check;
        both.extend(compile_errors(errors));
        Ok(TokenStream::from(TokenTree::Group(Group::new(
            Delimiter::Brace,
            both,
        ))))
    })
}

/// sqlx's own macro on the same literal and arguments, in a closure that is never called (so it
/// runs at build time and costs nothing at run time). Empty without feature `sqlx-check`.
fn check_tokens(form: Form, call: &Call) -> TokenStream {
    #[cfg(feature = "sqlx-check")]
    {
        let mut inner = TokenStream::new();
        if let Some((ty, _)) = &call.ty {
            inner.extend(ty.clone());
            inner.extend(tokens(","));
        }
        inner.extend([TokenTree::Literal(call.lit.clone())]);
        for a in &call.args {
            inner.extend(tokens(","));
            inner.extend(a.clone());
        }
        let Some(sqlx) = &call.sqlx else {
            return TokenStream::new();
        };
        let mut closure = tokens(&format!("{sqlx}::{}!", form.sqlx_macro()));
        closure.extend([paren(inner)]);
        closure.extend(tokens(";"));
        let mut out = tokens("#[allow(unused)] let _ = ||");
        out.extend([TokenTree::Group(Group::new(Delimiter::Brace, closure))]);
        out.extend(tokens(";"));
        out
    }
    #[cfg(not(feature = "sqlx-check"))]
    {
        let _ = (form, call);
        TokenStream::new()
    }
}

fn assemble(
    form: Form,
    call: &Call,
    manifest_dir: Option<&Path>,
    offline_env: Option<&str>,
) -> Result<TokenStream, Vec<Error>> {
    let span = call.lit.span();
    let Some(manifest) = manifest_dir else {
        return Err(vec![err(
            span,
            "no CARGO_MANIFEST_DIR: cannot find `.sqlx`",
        )]);
    };
    let Some(dir) = sqlx_offline::offline_dir(offline_env, manifest) else {
        return Err(vec![err(
            span,
            format!(
                "no `.sqlx` directory in {} or above, and SQLX_OFFLINE_DIR is not set: run `cargo sqlx prepare`",
                manifest.display()
            ),
        )]);
    };
    let described = sqlx_offline::load(&dir, &call.sql).map_err(|m| vec![err(span, m)])?;
    if call.args.len() != described.params.len() {
        return Err(vec![err(
            span,
            format!(
                "the statement takes {} parameter(s), {} argument(s) given",
                described.params.len(),
                call.args.len()
            ),
        )]);
    }
    let rel = call.krate.as_str();
    let mut out = TokenStream::new();

    // Rebuild when `cargo sqlx prepare` rewrites the file.
    let file = dir.join(sqlx_offline::file_name(&call.sql));
    out.extend(tokens(&format!(
        "const _: &[u8] = ::core::include_bytes!({:?});",
        file.to_string_lossy()
    )));

    // 1. sqlx's own check, never run.
    out.extend(check_tokens(form, call));

    // The decoder, and for the record form its struct.
    let decode: String = match form {
        Form::As => {
            let (_, ty) = call.ty.as_ref().expect("checked in parse");
            out.extend(tokens(&assertions(rel, ty, &described)));
            format!("<{ty} as {rel}::FromRow>::from_row")
        }
        Form::Scalar => {
            let [c] = described.columns.as_slice() else {
                return Err(vec![err(
                    span,
                    format!(
                        "sqlx_query_scalar! needs exactly one column, the statement returns {}",
                        described.columns.len()
                    ),
                )]);
            };
            let ty = if c.nullable {
                format!("::core::option::Option<{}>", c.rust_type)
            } else {
                c.rust_type.clone()
            };
            format!(
                "|row: &{rel}::Row| {rel}::row::read::<{ty}>(row, 0, {:?})",
                c.name
            )
        }
        Form::Record if described.columns.is_empty() => {
            format!("|_row: &{rel}::Row| ::core::result::Result::<(), {rel}::RelError>::Ok(())")
        }
        Form::Record => {
            let decl = sqlx_offline::record_struct("PersistantRecord", &described);
            out.extend(tokens(&format!("#[derive(Debug)] {decl}")));
            let for_derive = tokens(&format!("#[persistant(crate = {rel})] {decl}"));
            match crate::rows::derive_from_row(for_derive) {
                Ok(code) => out.extend(tokens(&code)),
                Err(e) => return Err(vec![err(span, e.message)]),
            }
            format!("<PersistantRecord as {rel}::FromRow>::from_row")
        }
    };

    // 2. the statement, with casts; 3. the bound run.
    let rewritten = rewrite_params(&call.sql, &described.params);
    let query = statement(call, &rewritten, manifest_dir);
    let params = {
        let items = call.args.iter().map(|a| {
            let mut item = tokens(&format!("{rel}::ToValue::to_value"));
            let mut inner = tokens("&");
            inner.extend([paren(a.clone())]);
            item.extend([paren(inner)]);
            item
        });
        let mut v = tokens("::std::vec!");
        v.extend([TokenTree::Group(Group::new(
            Delimiter::Bracket,
            comma_list(items),
        ))]);
        v
    };
    let mut bound = tokens(&format!("{rel}::Bound::new"));
    let bargs = comma_list([query, params, tokens(&decode)]);
    bound.extend([paren(bargs)]);
    out.extend(bound);
    Ok(TokenStream::from(TokenTree::Group(Group::new(
        Delimiter::Brace,
        out,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placeholder_is_cast_by_its_described_type() {
        let t = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            rewrite_params("SELECT 1 WHERE id = $1 AND n = $2", &t(&["Uuid", "Int8"])),
            "SELECT 1 WHERE id = CAST($1 AS uuid) AND n = $2"
        );
        assert_eq!(
            rewrite_params("WHERE a = $1 OR a = $1", &t(&["Timestamptz"])),
            "WHERE a = CAST($1 AS timestamptz) OR a = CAST($1 AS timestamptz)"
        );
    }

    #[test]
    fn strings_identifiers_and_comments_are_left_alone() {
        let t = vec!["Uuid".to_string()];
        let sql = "SELECT '$1', \"$1\" -- $1\n, /* $1 */ $1";
        assert_eq!(
            rewrite_params(sql, &t),
            "SELECT '$1', \"$1\" -- $1\n, /* $1 */ CAST($1 AS uuid)"
        );
        assert_eq!(
            rewrite_params("SELECT 'it''s $1' = $1", &t),
            "SELECT 'it''s $1' = CAST($1 AS uuid)"
        );
    }

    #[test]
    fn a_two_digit_placeholder_is_read_whole() {
        let mut t = vec!["Text".to_string(); 9];
        t.push("Date".to_string());
        assert_eq!(rewrite_params("$10 $1", &t), "CAST($10 AS date) $1");
    }

    const SQL: &str = "SELECT id, name FROM t WHERE id = $1";

    /// A `.sqlx` directory holding sqlx's description of `SQL` (and nothing else).
    fn offline(sql: &str, columns: &str, params: &str, nullable: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let doc = format!(
            r#"{{"db_name":"PostgreSQL","query":{q:?},"describe":{{"columns":[{columns}],"parameters":{{"Left":[{params}]}},"nullable":[{nullable}]}}}}"#,
            q = sql
        );
        std::fs::write(dir.path().join(sqlx_offline::file_name(sql)), doc).unwrap();
        dir
    }

    const TWO: &str = r#"{"ordinal":0,"name":"id","type_info":"Int8"},{"ordinal":1,"name":"name","type_info":"Text"}"#;

    fn run(form: Form, call: &str, dir: &tempfile::TempDir) -> Result<String, String> {
        let input: TokenStream = call.parse().unwrap();
        let out = expand_in(form, input, Some(dir.path()), dir.path().to_str());
        let text = out.to_string();
        if text.contains("compile_error") {
            Err(text)
        } else {
            Ok(text)
        }
    }

    #[test]
    fn a_statement_expands_to_a_bound_with_its_arguments_and_decoder() {
        let dir = offline(SQL, TWO, r#""Int8""#, "false,false");
        let out = run(Form::Record, &format!("{SQL:?}, 5_i64"), &dir).unwrap();
        let flat: String = out.split_whitespace().collect();
        assert!(
            flat.contains("::persistant::relational::Bound::new("),
            "{out}"
        );
        assert!(
            flat.contains("structPersistantRecord{pubid:i64,pubname:::std::string::String,}"),
            "{out}"
        );
        assert!(
            flat.contains("::persistant::relational::ToValue::to_value(&(5_i64))"),
            "{out}"
        );
        assert!(flat.contains("include_bytes!"), "{out}");
        // Without feature `sqlx-check` sqlx's macro is not named.
        assert_eq!(
            flat.contains("::sqlx::query!"),
            cfg!(feature = "sqlx-check"),
            "{out}"
        );
    }

    #[test]
    fn a_statement_changed_since_cargo_sqlx_prepare_has_no_file() {
        let dir = offline(SQL, TWO, r#""Int8""#, "false,false");
        let e = run(
            Form::Record,
            "\"SELECT id, name FROM t WHERE id = $2\", 5_i64",
            &dir,
        )
        .unwrap_err();
        assert!(e.contains("cargo sqlx prepare"), "{e}");
    }

    #[test]
    fn the_wrong_number_of_arguments_is_named() {
        let dir = offline(SQL, TWO, r#""Int8""#, "false,false");
        let e = run(Form::Record, &format!("{SQL:?}"), &dir).unwrap_err();
        assert!(
            e.contains("takes 1 parameter(s), 0 argument(s) given"),
            "{e}"
        );
    }

    #[test]
    fn a_scalar_needs_one_column_and_an_as_form_needs_its_struct() {
        let dir = offline(SQL, TWO, r#""Int8""#, "false,false");
        let e = run(Form::Scalar, &format!("{SQL:?}, 1_i64"), &dir).unwrap_err();
        assert!(e.contains("exactly one column"), "{e}");
        let e = run(Form::As, "", &dir).unwrap_err();
        assert!(e.contains("needs the struct first"), "{e}");
    }

    #[test]
    fn the_as_form_asserts_names_and_nullability_at_build_time() {
        let dir = offline(SQL, TWO, r#""Int8""#, "false,true");
        let out = run(Form::As, &format!("Row, {SQL:?}, 1_i64"), &dir).unwrap();
        let flat: String = out.split_whitespace().collect();
        assert!(flat.contains("check::fields_are::<Row>(2)"), "{out}");
        assert!(flat.contains("check::named::<Row>(1,\"name\")"), "{out}");
        assert!(flat.contains("check::null_fits::<Row>(1,true)"), "{out}");
    }

    #[test]
    fn a_uuid_parameter_is_cast_in_the_translated_text_only() {
        let dir = offline(SQL, TWO, r#""Uuid""#, "false,false");
        let out = run(Form::Record, &format!("{SQL:?}, id"), &dir).unwrap();
        // UniSQL carries the cast into the PostgreSQL text it embeds.
        assert!(out.contains("CAST") && out.contains("uuid"), "{out}");
    }
}
