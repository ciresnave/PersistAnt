// SPDX-License-Identifier: MIT OR Apache-2.0
//! The macro's work, on `proc_macro2` tokens so that it can be unit-tested (a `proc_macro::TokenStream`
//! exists only inside a macro call). A failing call yields `compile_error!` tokens, never a panic, so
//! rust-analyzer keeps the rest of the file.

use crate::checker::PersistantChecker;
use proc_macro2::{Delimiter, Group, Literal, Span, TokenStream, TokenTree};
use std::path::{Path, PathBuf};
use unisql::Schema;
use unisql_macro_core::{Input, Kind, Snapshot, expand_with};

/// The path the expansion names `Query`, `Rendered`, `Target` and `Binding` through.
const DEFAULT_CRATE: &str = "::persistant::relational::embed";

struct Error {
    span: Span,
    message: String,
}

fn err(span: Span, message: impl Into<String>) -> Error {
    Error {
        span,
        message: message.into(),
    }
}

/// Everything the call stated, with the span of each argument so an error can point at it.
#[derive(Default)]
struct Args {
    source: Option<(String, Span)>,
    targets: Option<(Vec<String>, Span)>,
    allow: Option<(Vec<String>, Span)>,
    requires: Option<(Vec<String>, Span)>,
    forbids: Option<(Vec<String>, Span)>,
    krate: Option<String>,
    schema: Option<(SchemaSpec, Span)>,
    /// `as T`: the struct the statement's columns are checked against.
    row: Option<(String, Span)>,
    sql: Option<(String, Span)>,
}

/// Where the macro's schema comes from.
enum SchemaSpec {
    /// `schema = none`: database-free checks only, stated.
    None,
    /// `schema = "path"`, relative to the crate's manifest directory.
    Path(String),
}

/// The snapshot the committed `persistant-migrate snapshot` writes, beside `Cargo.toml`.
const DEFAULT_SCHEMA: &str = "persistant-schema.json";

pub(crate) fn sql(input: TokenStream) -> TokenStream {
    // The one environment read, as UniSQL's design allows: the crate being compiled.
    let dir = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
    sql_in(input, dir.as_deref())
}

/// As [`sql`], with the manifest directory given (so tests need not touch the environment).
pub(crate) fn sql_in(input: TokenStream, manifest_dir: Option<&Path>) -> TokenStream {
    match build(input, manifest_dir) {
        Ok(tokens) => tokens,
        Err(errors) => compile_errors(errors),
    }
}

/// The expansion of `#[derive(FromRow)]`: an impl, or a `compile_error!` item at the offending token.
pub(crate) fn derive_from_row(input: TokenStream) -> TokenStream {
    match crate::rows::derive_from_row(input) {
        Ok(code) => code.parse().expect("the derive builds valid tokens"),
        Err(e) => {
            let mut call: TokenStream = "::core::compile_error!".parse().expect("fixed tokens");
            call.extend([TokenTree::Group(Group::new(
                Delimiter::Brace,
                TokenStream::from(TokenTree::Literal(Literal::string(&format!(
                    "persistant::FromRow: {}",
                    e.message
                )))),
            ))]);
            call.extend(";".parse::<TokenStream>().expect("fixed tokens"));
            respan(call, e.span)
        }
    }
}

fn compile_errors(errors: Vec<Error>) -> TokenStream {
    let mut body = TokenStream::new();
    for e in &errors {
        let mut call: TokenStream = "::core::compile_error!".parse().expect("fixed tokens");
        call.extend([TokenTree::Group(Group::new(
            Delimiter::Brace,
            TokenStream::from(TokenTree::Literal(Literal::string(&format!(
                "persistant::sql!: {}",
                e.message
            )))),
        ))]);
        call.extend(";".parse::<TokenStream>().expect("fixed tokens"));
        body.extend(respan(call, e.span));
    }
    // A block whose value is `!`, so it fits any expression position without a second type error.
    body.extend(
        "::core::unreachable!()"
            .parse::<TokenStream>()
            .expect("fixed tokens"),
    );
    TokenStream::from(TokenTree::Group(Group::new(Delimiter::Brace, body)))
}

fn respan(ts: TokenStream, span: Span) -> TokenStream {
    ts.into_iter()
        .map(|tt| match tt {
            TokenTree::Group(g) => {
                let mut n = Group::new(g.delimiter(), respan(g.stream(), span));
                n.set_span(span);
                TokenTree::Group(n)
            }
            mut other => {
                other.set_span(span);
                other
            }
        })
        .collect()
}

/// The schema the call asked for, and the file it came from (to be tracked by `include_bytes!`).
fn load_schema(
    spec: Option<&(SchemaSpec, Span)>,
    manifest_dir: Option<&Path>,
    source: &str,
) -> Result<(Option<Schema>, Option<PathBuf>), Error> {
    let (rel, span) = match spec {
        Some((SchemaSpec::None, _)) => return Ok((None, None)),
        Some((SchemaSpec::Path(p), span)) => (p.as_str(), *span),
        None => (DEFAULT_SCHEMA, Span::call_site()),
    };
    let Some(dir) = manifest_dir else {
        return Err(err(
            span,
            "CARGO_MANIFEST_DIR is not set, so the schema file cannot be found: write `schema = none` for database-free checks only",
        ));
    };
    let file = dir.join(rel);
    if spec.is_none() && !file.exists() {
        return Err(err(
            span,
            format!(
                "no schema: {DEFAULT_SCHEMA} is not in {}. Run `persistant-migrate snapshot` and commit the file, name another with `schema = \"path\"`, or write `schema = none` for database-free checks only",
                dir.display()
            ),
        ));
    }
    let text = std::fs::read_to_string(&file).map_err(|e| {
        err(
            span,
            format!("cannot read the schema {}: {e}", file.display()),
        )
    })?;
    let schema = match file.extension().and_then(|e| e.to_str()) {
        Some("json") => {
            Snapshot::from_json(&text)
                .map_err(|e| err(span, format!("{}: {e}", file.display())))?
                .schema
        }
        Some("sql") => {
            unisql::schema_from_ddl(&text, source)
                .map_err(|e| err(span, format!("{}: {e}", file.display())))?
                .schema
        }
        _ => {
            return Err(err(
                span,
                format!(
                    "the schema {} must be a .json snapshot or a .sql DDL file",
                    file.display()
                ),
            ));
        }
    };
    Ok((Some(schema), Some(file)))
}

fn build(input: TokenStream, manifest_dir: Option<&Path>) -> Result<TokenStream, Vec<Error>> {
    let args = parse_args(input)?;
    let mut missing = Vec::new();
    let sql_span = args.sql.as_ref().map_or_else(Span::call_site, |s| s.1);
    let Some((source, _)) = args.source.clone() else {
        missing.push(err(
            Span::call_site(),
            "no `source = ..` given: name the dialect the statement is written in",
        ));
        return Err(missing);
    };
    let Some((targets, _)) = args.targets.clone() else {
        return Err(vec![err(
            Span::call_site(),
            "no `targets = [..]` given: a call translates for exactly the targets it lists",
        )]);
    };
    let Some((sql, _)) = args.sql.clone() else {
        return Err(vec![err(
            Span::call_site(),
            "no statement given: the last argument must be the SQL as a string literal",
        )]);
    };
    let (schema, schema_file) =
        load_schema(args.schema.as_ref(), manifest_dir, &source).map_err(|e| vec![e])?;
    // `as T`: read the SELECT list against the schema, before the schema moves into the input.
    let rows = match (&args.row, &schema) {
        (None, _) => None,
        (Some((ty, span)), None) => {
            return Err(vec![err(
                *span,
                format!(
                    "`as {ty}` needs a schema: the columns are checked against it (`schema = none` has nothing to check them with)"
                ),
            )]);
        }
        (Some((ty, _)), Some(schema)) => {
            let krate = args.krate.as_deref().unwrap_or(DEFAULT_CRATE);
            let Some(rel) = krate.strip_suffix("::embed") else {
                return Err(vec![err(
                    sql_span,
                    "with `as T`, `crate = ..` must name the `embed` module of the relational crate (so the checks can be found beside it)",
                )]);
            };
            let cols =
                crate::rows::columns(&sql, &source, schema).map_err(|m| vec![err(sql_span, m)])?;
            Some((
                rel.to_string(),
                ty.clone(),
                crate::rows::assertions(rel, ty, &cols),
            ))
        }
    };
    let input = Input {
        source,
        targets,
        allow: args.allow.as_ref().map(|a| a.0.clone()).unwrap_or_default(),
        requires: args
            .requires
            .as_ref()
            .map(|a| a.0.clone())
            .unwrap_or_default(),
        forbids: args
            .forbids
            .as_ref()
            .map(|a| a.0.clone())
            .unwrap_or_default(),
        sql,
        schema,
    };
    match expand_with(&input, &[&PersistantChecker]) {
        Ok(expansion) => {
            let path = args.krate.as_deref().unwrap_or(DEFAULT_CRATE);
            let mut text = render(path, &expansion);
            if let Some((rel, ty, checks)) = &rows {
                text = format!("{{ {checks} {rel}::TypedSql::<{ty}>::new({text}) }}");
            }
            if let Some(file) = schema_file {
                // Reading the file is not tracked by cargo; including its bytes is, so the crate is
                // rebuilt when the snapshot changes. A block, so it fits an expression position.
                text = format!(
                    "{{ const _: &[u8] = ::core::include_bytes!({:?}); {text} }}",
                    file.to_string_lossy()
                );
            }
            text.parse::<TokenStream>().map_err(|e| {
                vec![err(
                    sql_span,
                    format!("internal error building the expansion: {e}"),
                )]
            })
        }
        Err(problems) => Err(problems
            .into_iter()
            .map(|p| {
                let span = match p.kind {
                    Kind::Source => args.source.as_ref().map(|a| a.1),
                    Kind::Targets => args.targets.as_ref().map(|a| a.1),
                    Kind::Allow => args.allow.as_ref().map(|a| a.1),
                    Kind::Requires => args.requires.as_ref().map(|a| a.1),
                    Kind::Forbids => args.forbids.as_ref().map(|a| a.1),
                    Kind::Syntax | Kind::Placeholders | Kind::Refused | Kind::Catalog => {
                        Some(sql_span)
                    }
                }
                .unwrap_or(sql_span);
                err(span, p.message)
            })
            .collect()),
    }
}

/// The Rust source of the `Query` value, with every target's text embedded as a constant.
fn render(path: &str, e: &unisql_macro_core::Expansion) -> String {
    let constructs: Vec<String> = e.constructs.iter().map(|c| format!("{c:?}")).collect();
    let rendered: Vec<String> = e
        .rendered
        .iter()
        .map(|r| {
            let target = match r.target.as_str() {
                "postgres" => "Postgres",
                "sqlite" => "Sqlite",
                "mysql" => "Mysql",
                "mariadb" => "Mariadb",
                other => unreachable!("expand accepted the unknown target {other}"),
            };
            let order: Vec<String> = r.param_order.iter().map(|n| n.to_string()).collect();
            let bindings: Vec<String> = r
                .param_bindings
                .iter()
                .map(|b| {
                    let v = match b.name() {
                        "scalar" => "Scalar",
                        "native_array" => "NativeArray",
                        "json_array_text" => "JsonArrayText",
                        other => unreachable!("unknown binding {other}"),
                    };
                    format!("{path}::Binding::{v}")
                })
                .collect();
            format!(
                "{path}::Rendered {{ target: {path}::Target::{target}, sql: {sql:?}, \
                 param_order: &[{order}], param_bindings: &[{bindings}] }}",
                sql = r.sql,
                order = order.join(", "),
                bindings = bindings.join(", ")
            )
        })
        .collect();
    format!(
        "{path}::Query {{ params: {params}, constructs: &[{constructs}], rendered: &[{rendered}] }}",
        params = e.params,
        constructs = constructs.join(", "),
        rendered = rendered.join(", ")
    )
}

// ---- argument parsing ----------------------------------------------------------------------

fn split_commas(tokens: Vec<TokenTree>) -> Vec<Vec<TokenTree>> {
    let mut out = vec![Vec::new()];
    for t in tokens {
        match &t {
            TokenTree::Punct(p) if p.as_char() == ',' => out.push(Vec::new()),
            _ => out.last_mut().expect("never empty").push(t),
        }
    }
    out
}

fn parse_args(input: TokenStream) -> Result<Args, Vec<Error>> {
    let mut segments = split_commas(input.into_iter().collect());
    if segments.last().is_some_and(Vec::is_empty) {
        segments.pop(); // a trailing comma
    }
    let mut args = Args::default();
    let mut errors = Vec::new();
    let count = segments.len();
    for (i, seg) in segments.into_iter().enumerate() {
        let Some(first) = seg.first() else {
            errors.push(err(
                Span::call_site(),
                "an empty argument (two commas in a row)",
            ));
            continue;
        };
        let span = first.span();
        // The statement: a lone string literal, and it must come last.
        if let (TokenTree::Literal(lit), 1) = (first, seg.len()) {
            if i + 1 != count {
                errors.push(err(span, "the statement must be the last argument"));
                continue;
            }
            match string_value(lit) {
                Some(s) if args.sql.is_none() => args.sql = Some((s, span)),
                Some(_) => errors.push(err(span, "the statement is given twice")),
                None => errors.push(err(span, "the statement must be a string literal")),
            }
            continue;
        }
        // `as T`: the struct the columns are checked against
        if let TokenTree::Ident(kw) = first
            && kw == "as"
            && seg.len() >= 2
        {
            let ty: TokenStream = seg[1..].iter().cloned().collect();
            if args.row.is_some() {
                errors.push(err(span, "`as` is given twice"));
            } else {
                args.row = Some((ty.to_string(), span));
            }
            continue;
        }
        // `name = value`
        let named = match (&seg[..], seg.get(1)) {
            ([TokenTree::Ident(name), ..], Some(TokenTree::Punct(eq))) if eq.as_char() == '=' => {
                Some(name.to_string())
            }
            _ => None,
        };
        let Some(name) = named else {
            errors.push(err(
                span,
                "expected `name = value` (source, targets, allow, requires, forbids, schema, crate), `as Type`, or the statement as a string literal",
            ));
            continue;
        };
        let value: Vec<TokenTree> = seg[2..].to_vec();
        if value.is_empty() {
            errors.push(err(span, format!("`{name} =` has no value")));
            continue;
        }
        let vspan = value[0].span();
        let dup = |errors: &mut Vec<Error>| {
            errors.push(err(span, format!("`{name}` is given twice")));
        };
        match name.as_str() {
            "source" => match one_name(&value, "a dialect name") {
                Ok(v) if args.source.is_none() => args.source = Some((v, vspan)),
                Ok(_) => dup(&mut errors),
                Err(e) => errors.push(e),
            },
            "targets" | "allow" | "requires" | "forbids" => match name_list(&value) {
                Ok(v) => {
                    let slot = match name.as_str() {
                        "targets" => &mut args.targets,
                        "allow" => &mut args.allow,
                        "requires" => &mut args.requires,
                        _ => &mut args.forbids,
                    };
                    if slot.is_some() {
                        dup(&mut errors);
                    } else {
                        *slot = Some((v, vspan));
                    }
                }
                Err(e) => errors.push(e),
            },
            "schema" => {
                let spec = match &value[..] {
                    [TokenTree::Ident(i)] if i == "none" => Ok(SchemaSpec::None),
                    [TokenTree::Literal(l)] => string_value(l).map(SchemaSpec::Path).ok_or_else(|| {
                        err(l.span(), "expected `none` or a path in a string literal")
                    }),
                    _ => Err(err(vspan, "expected `none` or a path in a string literal")),
                };
                match spec {
                    Ok(_) if args.schema.is_some() => dup(&mut errors),
                    Ok(spec) => args.schema = Some((spec, vspan)),
                    Err(e) => errors.push(e),
                }
            }
            "crate" => {
                if args.krate.is_some() {
                    dup(&mut errors);
                } else {
                    args.krate = Some(
                        value
                            .into_iter()
                            .collect::<TokenStream>()
                            .to_string()
                            .replace(' ', ""),
                    );
                }
            }
            other => errors.push(err(
                span,
                format!(
                    "unknown argument `{other}` (known: source, targets, allow, requires, forbids, schema, crate)"
                ),
            )),
        }
    }
    if errors.is_empty() {
        Ok(args)
    } else {
        Err(errors)
    }
}

fn one_name(tokens: &[TokenTree], what: &str) -> Result<String, Error> {
    match tokens {
        [TokenTree::Ident(i)] => Ok(i.to_string().trim_start_matches("r#").to_string()),
        [TokenTree::Literal(l)] => string_value(l).ok_or_else(|| {
            err(
                l.span(),
                format!("expected {what}, a name or a string literal"),
            )
        }),
        [first, ..] => Err(err(first.span(), format!("expected {what}"))),
        [] => Err(err(Span::call_site(), format!("expected {what}"))),
    }
}

fn name_list(tokens: &[TokenTree]) -> Result<Vec<String>, Error> {
    let [TokenTree::Group(g)] = tokens else {
        let span = tokens.first().map_or_else(Span::call_site, TokenTree::span);
        return Err(err(
            span,
            "expected a list in square brackets, like `[a, b]`",
        ));
    };
    if g.delimiter() != Delimiter::Bracket {
        return Err(err(
            g.span(),
            "expected a list in square brackets, like `[a, b]`",
        ));
    }
    let mut parts = split_commas(g.stream().into_iter().collect());
    if parts.last().is_some_and(Vec::is_empty) {
        parts.pop();
    }
    parts
        .iter()
        .map(|p| one_name(p, "a name or a string literal"))
        .collect()
}

/// The value of a plain or raw string literal; `None` for anything else (numbers, chars, byte strings).
fn string_value(lit: &Literal) -> Option<String> {
    unescape(&lit.to_string())
}

/// As [`string_value`], on the literal's source text.
fn unescape(text: &str) -> Option<String> {
    if let Some(rest) = text.strip_prefix('r') {
        let hashes = rest.bytes().take_while(|b| *b == b'#').count();
        let inner = rest.get(hashes..)?.strip_prefix('"')?;
        let end = format!("\"{}", "#".repeat(hashes));
        return Some(inner.strip_suffix(end.as_str())?.to_string());
    }
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            'x' => {
                let hex: String = [chars.next()?, chars.next()?].iter().collect();
                out.push(char::from(
                    u8::from_str_radix(&hex, 16).ok().filter(|b| *b < 0x80)?,
                ));
            }
            'u' => {
                if chars.next()? != '{' {
                    return None;
                }
                let mut hex = String::new();
                loop {
                    match chars.next()? {
                        '}' => break,
                        '_' => {}
                        h => hex.push(h),
                    }
                }
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            // a backslash at the end of a line skips the newline and the leading whitespace of the next
            '\n' | '\r' => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> String {
        // These tests are about syntax and translation, not schemas: they state `schema = none`.
        let src = if src.contains("schema") {
            src.to_string()
        } else {
            format!("schema = none, {src}")
        };
        sql_in(src.parse().expect("test tokens parse"), None).to_string()
    }

    fn run_in(src: &str, dir: &Path) -> String {
        sql_in(src.parse().expect("test tokens parse"), Some(dir)).to_string()
    }

    fn ok(src: &str) -> String {
        let out = run(src);
        assert!(
            !out.contains("compile_error"),
            "expected a clean expansion: {out}"
        );
        out
    }

    fn fails(src: &str, needle: &str) {
        let out = run(src);
        assert!(
            out.contains("compile_error"),
            "expected a compile error: {out}"
        );
        assert!(out.contains(needle), "expected {needle:?} in {out}");
    }

    #[test]
    fn a_clean_call_embeds_the_text_of_every_target() {
        let out = ok(r#"source = postgres, targets = [sqlite, mysql], "SELECT $1 + $1""#);
        // `TokenStream::to_string` puts spaces between tokens; compare without them.
        let flat = out.replace(' ', "");
        assert!(
            flat.starts_with("::persistant::relational::embed::Query{params:1,"),
            "{out}"
        );
        assert!(
            flat.contains("target:::persistant::relational::embed::Target::Sqlite"),
            "{out}"
        );
        assert!(
            flat.contains("target:::persistant::relational::embed::Target::Mysql"),
            "{out}"
        );
        assert!(
            !flat.contains("Target::Postgres"),
            "only named targets are embedded: {out}"
        );
        // the translated text of each target, and the placeholder reuse in the order table
        assert!(flat.contains(r#"sql:"SELECT?1+?1""#), "{out}");
        assert!(flat.contains(r#"sql:"SELECT?+?""#), "{out}");
        assert!(flat.contains("param_order:&[1,1]"), "{out}");
    }

    #[test]
    fn arguments_may_come_in_any_order_with_a_trailing_comma() {
        ok(r#"targets = [sqlite], source = postgres, "SELECT 1","#);
        ok(r#"source = "postgres", targets = ["sqlite"], "SELECT 1""#);
    }

    #[test]
    fn source_is_required() {
        fails(r#"targets = [sqlite], "SELECT 1""#, "no `source");
        ok(r#"source = postgres, targets = [sqlite], "SELECT 1""#);
    }

    #[test]
    fn targets_are_required_known_and_unique() {
        fails(r#"source = postgres, "SELECT 1""#, "no `targets");
        fails(
            r#"source = postgres, targets = [], "SELECT 1""#,
            "no targets named",
        );
        fails(
            r#"source = postgres, targets = [oracle], "SELECT 1""#,
            "unknown target dialect `oracle`",
        );
        fails(
            r#"source = postgres, targets = [sqlite, sqlite], "SELECT 1""#,
            "named twice",
        );
        ok(r#"source = postgres, targets = [sqlite, mariadb], "SELECT 1""#);
    }

    #[test]
    fn an_unknown_source_is_named() {
        fails(
            r#"source = oracle, targets = [sqlite], "SELECT 1""#,
            "unknown source dialect `oracle`",
        );
    }

    #[test]
    fn the_statement_is_required_last_and_a_string() {
        fails(
            r#"source = postgres, targets = [sqlite]"#,
            "no statement given",
        );
        fails(
            r#"source = postgres, "SELECT 1", targets = [sqlite]"#,
            "must be the last argument",
        );
        fails(
            r#"source = postgres, targets = [sqlite], 42"#,
            "must be a string literal",
        );
        fails(
            r#"source = postgres, targets = [sqlite], select"#,
            "expected `name = value`",
        );
        fails(
            r#"source = postgres, targets = [sqlite], b"SELECT 1""#,
            "must be a string literal",
        );
    }

    #[test]
    fn syntax_errors_are_compile_errors() {
        fails(r#"source = postgres, targets = [sqlite], "SELEC 1""#, "");
        ok(r#"source = postgres, targets = [sqlite], "SELECT 1""#);
    }

    #[test]
    fn a_skipped_placeholder_is_a_compile_error() {
        fails(
            r#"source = postgres, targets = [sqlite], allow = [text_collation], "SELECT a FROM t WHERE x = $1 AND y = $3""#,
            "none skipped",
        );
        ok(
            r#"source = postgres, targets = [sqlite], allow = [text_collation], "SELECT a FROM t WHERE x = $1 AND y = $2""#,
        );
    }

    #[test]
    fn a_refusing_target_is_named() {
        fails(
            r#"source = postgres, targets = [postgres, sqlite], "SELECT md5($1)""#,
            "sqlite",
        );
        ok(r#"source = postgres, targets = [postgres], "SELECT md5($1)""#);
    }

    #[test]
    fn a_conditional_needs_its_allowance() {
        let sql = r#""SELECT a FROM t WHERE name < $1""#;
        fails(
            &format!("source = postgres, targets = [mysql], {sql}"),
            "text_collation",
        );
        ok(&format!(
            "source = postgres, targets = [mysql], allow = [text_collation], {sql}"
        ));
        fails(
            &format!("source = postgres, targets = [mysql], allow = [text_collatoin], {sql}"),
            "unknown allowance `text_collatoin`",
        );
    }

    #[test]
    fn requires_and_forbids_pin_the_constructs() {
        let up = r#""INSERT INTO kv (k, v) VALUES ($1, $2) ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v""#;
        let base = "source = postgres, targets = [sqlite], allow = [upsert_any_unique_key]";
        ok(&format!("{base}, requires = [upsert], {up}"));
        fails(
            &format!("{base}, requires = [returning], {up}"),
            "required construct `returning`",
        );
        fails(
            &format!("{base}, forbids = [upsert], {up}"),
            "forbidden construct `upsert`",
        );
        ok(&format!("{base}, forbids = [returning], {up}"));
    }

    #[test]
    fn argument_mistakes_are_named() {
        fails(
            r#"source = postgres, source = sqlite, targets = [sqlite], "SELECT 1""#,
            "`source` is given twice",
        );
        fails(
            r#"source = postgres, targets = [sqlite], colour = red, "SELECT 1""#,
            "unknown argument `colour`",
        );
        fails(
            r#"source = postgres, targets = sqlite, "SELECT 1""#,
            "square brackets",
        );
        fails(
            r#"source = , targets = [sqlite], "SELECT 1""#,
            "has no value",
        );
        fails(
            r#"source = postgres,, targets = [sqlite], "SELECT 1""#,
            "empty argument",
        );
    }

    #[test]
    fn several_errors_are_all_reported() {
        let out = run(r#"source = oracle, targets = [mongo], "SELECT 1""#);
        assert!(out.matches("compile_error").count() >= 2, "{out}");
    }

    #[test]
    fn the_expansion_path_can_be_overridden() {
        let out = ok(r#"source = postgres, targets = [sqlite], crate = ::my::embed, "SELECT 1""#);
        assert!(out.contains(":: my :: embed :: Query"), "{out}");
        assert!(!out.contains("persistant"), "{out}");
    }

    #[test]
    fn string_literals_are_read_as_rust_reads_them() {
        // the literal's source text, as the compiler hands it to a macro
        let one = unescape;
        assert_eq!(one(r#""SELECT 1""#).as_deref(), Some("SELECT 1"));
        assert_eq!(
            one(r#""a\nb\t\"c\" \\ \x41 \u{e9}""#).as_deref(),
            Some("a\nb\t\"c\" \\ A \u{e9}")
        );
        assert_eq!(one("\"a \\\n   b\"").as_deref(), Some("a b"));
        assert_eq!(one(r##"r#"it's "raw""#"##).as_deref(), Some("it's \"raw\""));
        assert_eq!(one(r#"r"x""#).as_deref(), Some("x"));
        for no in ["42", "'c'", r#"b"x""#, r#""\q""#, r#""\x80""#] {
            assert_eq!(one(no), None, "{no}");
        }
    }

    #[test]
    fn a_multi_line_statement_keeps_its_text() {
        let out = ok("source = postgres, targets = [sqlite], \"SELECT 1\n  FROM t\"");
        // the translator normalises whitespace and quoting; the newline in the source is not a problem
        assert!(out.replace(' ', "").contains(r#"FROM\"t\""#), "{out}");
    }

    // ---- schemas -------------------------------------------------------------------------

    const DDL: &str = "CREATE TABLE users (id BIGINT NOT NULL PRIMARY KEY, email TEXT NOT NULL, \
                       note TEXT, age INTEGER DEFAULT 7, UNIQUE (email));";

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(d.path().join(name), text).unwrap();
        }
        d
    }

    fn call(schema: &str, sql: &str) -> String {
        format!("source = postgres, targets = [sqlite], {schema}, \"{sql}\"")
    }

    fn ok_in(src: &str, dir: &Path) -> String {
        let out = run_in(src, dir);
        assert!(
            !out.contains("compile_error"),
            "expected a clean expansion: {out}"
        );
        out
    }

    fn fails_in(src: &str, dir: &Path, needle: &str) {
        let out = run_in(src, dir);
        assert!(
            out.contains("compile_error"),
            "expected a compile error: {out}"
        );
        assert!(out.contains(needle), "expected {needle:?} in {out}");
    }

    #[test]
    fn no_schema_is_an_error_not_a_quiet_downgrade() {
        let empty = dir_with(&[]);
        let plain = r#"source = postgres, targets = [sqlite], "SELECT 1""#;
        fails_in(
            plain,
            empty.path(),
            "no schema: persistant-schema.json is not in",
        );
        // The controls: opting out in words, or having the file, builds.
        ok_in(
            r#"source = postgres, targets = [sqlite], schema = none, "SELECT 1""#,
            empty.path(),
        );
        let with_default = dir_with(&[("persistant-schema.json", &snapshot_json())]);
        let out = ok_in(plain, with_default.path());
        assert!(
            out.contains("include_bytes"),
            "the file is tracked by cargo: {out}"
        );
        // No manifest directory at all (a tool that sets no environment) is named, not a panic.
        assert!(
            sql_in(plain.parse().unwrap(), None)
                .to_string()
                .contains("CARGO_MANIFEST_DIR")
        );
    }

    fn snapshot_json() -> String {
        let schema = unisql::schema_from_ddl(DDL, "postgres").unwrap().schema;
        Snapshot::new(schema).to_json()
    }

    #[test]
    fn a_not_null_column_left_out_of_an_insert_is_caught() {
        let d = dir_with(&[("s.sql", DDL)]);
        let bad = call(r#"schema = "s.sql""#, "INSERT INTO users (id) VALUES ($1)");
        fails_in(
            &bad,
            d.path(),
            "column `email` of `users` is NOT NULL with no default",
        );
        // The twin: the same statement with the fault removed. `note` (nullable) and `age` (has a
        // default) are left out in both and are fine.
        let good = call(
            r#"schema = "s.sql""#,
            "INSERT INTO users (id, email) VALUES ($1, $2)",
        );
        let out = ok_in(&good, d.path());
        assert!(
            out.contains("Target :: Sqlite") || out.contains("Target::Sqlite"),
            "{out}"
        );
    }

    #[test]
    fn on_conflict_must_name_a_stated_unique_key() {
        let d = dir_with(&[("s.sql", DDL)]);
        let stmt = |target: &str| {
            call(
                r#"schema = "s.sql""#,
                &format!(
                    "INSERT INTO users (id, email) VALUES ($1, $2) ON CONFLICT ({target}) DO NOTHING"
                ),
            )
        };
        fails_in(
            &stmt("note"),
            d.path(),
            "ON CONFLICT (note) is not a unique key of `users`",
        );
        // The twins: each stated key is accepted (the primary key and the unique constraint).
        ok_in(&stmt("id"), d.path());
        ok_in(&stmt("email"), d.path());
        // A key list marked unreliable (a partial or expression unique index exists) is unknown, so a
        // conflict target the list does not show is NOT reported.
        let mut schema = unisql::schema_from_ddl(DDL, "postgres").unwrap().schema;
        schema.mark_keys_unreliable(vec!["users".to_string()]);
        std::fs::write(d.path().join("u.json"), Snapshot::new(schema).to_json()).unwrap();
        let unreliable = call(
            r#"schema = "u.json""#,
            "INSERT INTO users (id, email) VALUES ($1, $2) ON CONFLICT (note) DO NOTHING",
        );
        ok_in(&unreliable, d.path());
    }

    #[test]
    fn unisqls_own_name_checks_run_through_the_macro() {
        let d = dir_with(&[("s.sql", DDL)]);
        fails_in(
            &call(
                r#"schema = "s.sql""#,
                "INSERT INTO users (id, email, nope) VALUES ($1, $2, $3)",
            ),
            d.path(),
            "has no column `nope`",
        );
        fails_in(
            &call(
                r#"schema = "s.sql""#,
                "INSERT INTO users (id, email) VALUES ($1)",
            ),
            d.path(),
            "VALUES row 1 has 1 value(s)",
        );
        ok_in(
            &call(
                r#"schema = "s.sql""#,
                "INSERT INTO users (id, email) VALUES ($1, $2)",
            ),
            d.path(),
        );
    }

    #[test]
    fn a_known_numeric_column_needs_no_text_collation_allowance() {
        let d = dir_with(&[("s.sql", DDL)]);
        let sql = "SELECT email FROM users WHERE id < $1";
        let with =
            |schema: &str| format!(r#"source = postgres, targets = [mysql], {schema}, "{sql}""#);
        // Without a schema the comparison is taken for text and MySQL's collation is a risk.
        fails_in(&with("schema = none"), d.path(), "text_collation");
        // With one, `id` is known to be a number.
        ok_in(&with(r#"schema = "s.sql""#), d.path());
    }

    #[test]
    fn a_json_snapshot_is_read_and_a_newer_one_refused() {
        let d = dir_with(&[("snap.json", &snapshot_json())]);
        fails_in(
            &call(
                r#"schema = "snap.json""#,
                "INSERT INTO users (id) VALUES ($1)",
            ),
            d.path(),
            "NOT NULL with no default",
        );
        let newer = snapshot_json().replace("\"format\": 1", "\"format\": 99");
        std::fs::write(d.path().join("new.json"), newer).unwrap();
        fails_in(
            &call(r#"schema = "new.json""#, "SELECT 1"),
            d.path(),
            "newer",
        );
    }

    #[test]
    fn schema_argument_mistakes_are_named() {
        let d = dir_with(&[("s.sql", DDL), ("s.txt", "x")]);
        fails_in(
            &call(r#"schema = "gone.json""#, "SELECT 1"),
            d.path(),
            "cannot read the schema",
        );
        fails_in(
            &call(r#"schema = "s.txt""#, "SELECT 1"),
            d.path(),
            "must be a .json snapshot or a .sql DDL file",
        );
        fails_in(
            &call("schema = 5", "SELECT 1"),
            d.path(),
            "expected `none` or a path",
        );
        fails_in(
            &call(r#"schema = "s.sql", schema = none"#, "SELECT 1"),
            d.path(),
            "`schema` is given twice",
        );
        ok_in(&call(r#"schema = "s.sql""#, "SELECT 1"), d.path());
    }

    // ---- typed rows (`as T`) --------------------------------------------------------------

    fn typed(schema: &str, sql: &str) -> String {
        format!("as User, source = postgres, targets = [sqlite], {schema}, \"{sql}\"")
    }

    #[test]
    fn as_t_builds_a_typed_statement_with_assertions_for_each_column() {
        let d = dir_with(&[("s.sql", DDL)]);
        let out = ok_in(
            &typed(
                r#"schema = "s.sql""#,
                "SELECT id, email, note FROM users WHERE id = $1",
            ),
            d.path(),
        );
        let flat = out.replace(' ', "");
        assert!(
            flat.contains("::persistant::relational::TypedSql::<User>::new("),
            "{out}"
        );
        assert!(flat.contains("fields_are::<User>(3)"), "{out}");
        assert!(flat.contains(r#"named::<User>(2,"note")"#), "{out}");
        assert!(
            flat.contains("null_fits::<User>(2,true)"),
            "the nullable column asks for an Option: {out}"
        );
        assert!(flat.contains("null_fits::<User>(0,false)"), "{out}");
    }

    #[test]
    fn as_t_needs_a_schema_and_a_plain_select() {
        let d = dir_with(&[("s.sql", DDL)]);
        fails_in(
            &typed("schema = none", "SELECT id FROM users"),
            d.path(),
            "`as User` needs a schema",
        );
        fails_in(
            &typed(r#"schema = "s.sql""#, "SELECT * FROM users"),
            d.path(),
            "list the columns",
        );
        fails_in(
            &typed(r#"schema = "s.sql""#, "SELECT id + 1 FROM users"),
            d.path(),
            "column 1 is an expression",
        );
        fails_in(
            &typed(r#"schema = "s.sql""#, "SELECT nope FROM users"),
            d.path(),
            "has no column `nope`",
        );
        // The twin: naming and typing the expression, or listing the columns, builds.
        ok_in(
            &typed(
                r#"schema = "s.sql""#,
                r#"SELECT id + 1 AS \"next: i64\" FROM users"#,
            ),
            d.path(),
        );
        // `crate = ..` has to name the embed module so the checks can be found beside it.
        fails_in(
            "as User, crate = ::elsewhere, source = postgres, targets = [sqlite], schema = \"s.sql\", \"SELECT id FROM users\"",
            d.path(),
            "must name the `embed` module",
        );
        ok_in(
            "as User, crate = ::mine::embed, source = postgres, targets = [sqlite], schema = \"s.sql\", \"SELECT id FROM users\"",
            d.path(),
        );
        fails_in(
            "as User, as Other, source = postgres, targets = [sqlite], schema = \"s.sql\", \"SELECT id FROM users\"",
            d.path(),
            "`as` is given twice",
        );
    }
}
