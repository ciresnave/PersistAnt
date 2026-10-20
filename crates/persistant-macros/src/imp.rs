// SPDX-License-Identifier: MIT OR Apache-2.0
//! The macro's work, on `proc_macro2` tokens so that it can be unit-tested (a `proc_macro::TokenStream`
//! exists only inside a macro call). A failing call yields `compile_error!` tokens, never a panic, so
//! rust-analyzer keeps the rest of the file.

use proc_macro2::{Delimiter, Group, Literal, Span, TokenStream, TokenTree};
use unisql_macro_core::{Input, Kind, expand};

/// The path the expansion names `Query`, `Rendered`, `Target` and `Binding` through.
const DEFAULT_CRATE: &str = "::persistant::embed";

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
    sql: Option<(String, Span)>,
}

pub(crate) fn sql(input: TokenStream) -> TokenStream {
    match build(input) {
        Ok(tokens) => tokens,
        Err(errors) => compile_errors(errors),
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

fn build(input: TokenStream) -> Result<TokenStream, Vec<Error>> {
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
    };
    match expand(&input) {
        Ok(expansion) => {
            let path = args.krate.as_deref().unwrap_or(DEFAULT_CRATE);
            render(path, &expansion)
                .parse::<TokenStream>()
                .map_err(|e| {
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
                    Kind::Syntax | Kind::Placeholders | Kind::Refused => Some(sql_span),
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
                "expected `name = value` (source, targets, allow, requires, forbids, crate) or the statement as a string literal",
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
            "crate" => {
                if args.krate.is_some() {
                    dup(&mut errors);
                } else {
                    args.krate = Some(value.into_iter().collect::<TokenStream>().to_string());
                }
            }
            other => errors.push(err(
                span,
                format!(
                    "unknown argument `{other}` (known: source, targets, allow, requires, forbids, crate)"
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
        sql(src.parse().expect("test tokens parse")).to_string()
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
            flat.starts_with("::persistant::embed::Query{params:1,"),
            "{out}"
        );
        assert!(
            flat.contains("target:::persistant::embed::Target::Sqlite"),
            "{out}"
        );
        assert!(
            flat.contains("target:::persistant::embed::Target::Mysql"),
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
}
