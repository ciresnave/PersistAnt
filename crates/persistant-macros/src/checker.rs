// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt's catalog check, run by `unisql-macro-core` over the parsed statement after UniSQL's own
//! name checks (`SchemaChecker`: unknown column, `INSERT` arity, a column listed twice).
//!
//! The rule for every check here is that it errors **only when certain**. A table or column the
//! snapshot does not state, a table whose column list is not marked complete, a key list marked
//! unreliable (a partial or expression unique index exists), a flag the reader could not tell
//! (`None`): all are unknown and pass. What it checks, and nothing else:
//!
//! * an `INSERT` that leaves out a `NOT NULL` column with no default (the server would refuse it);
//! * an `ON CONFLICT (cols)` whose columns are not, as a set, one of the table's stated unique keys
//!   (PostgreSQL and SQLite refuse a conflict target no unique index covers).
//!
//! * a comparison whose two operands PostgreSQL has no operator for, between a plain column of the one
//!   table of a `SELECT` / `UPDATE` / `DELETE` and a literal or another column of that table (a text
//!   column against a number, a number against a boolean, and so on). **Only when the source is
//!   PostgreSQL**: SQLite and MySQL convert between those classes silently, so there it is no fault.
//!   A string literal against a numeric column is never reported (PostgreSQL reads `'5'` as an
//!   untyped literal and converts it).
//!
//! * a declared argument type (`params = (..)`) whose class differs from the plain column its `$n` meets
//!   in `WHERE col = $n`, `INSERT INTO t (cols) VALUES ($1, ..)` or `UPDATE t SET col = $n`: text against
//!   a number or a boolean, and the reverse. A boolean against a number is a fault only when the source
//!   is PostgreSQL (SQLite and MySQL store booleans as integers). A type this file does not know
//!   (`Uuid`, a date, a JSON value, an alias) is unknown and passes; nullability is not checked.
//!
//! Not checked, and said so in the macro's documentation: the range of an argument, the types of
//! selected columns (stage 6), an argument anywhere else than those three positions, joins, subqueries, CTEs, views, other schemas, and any operand that is not a
//! plain column or a literal.

use unisql::ast::{self, BinaryOp, Expr, InsertSource, Literal, Name, SetExpr, TableRef};
use unisql::{ColumnClass, Schema};
use unisql_macro_core::{CheckContext, Checker, Kind, Problem};

pub(crate) struct PersistantChecker;

fn problem(message: String) -> Problem {
    Problem {
        kind: Kind::Catalog,
        message,
    }
}

fn same_set(a: &[String], b: &[String]) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|x| b.iter().any(|y| y.eq_ignore_ascii_case(x)))
}

/// The class a declared Rust argument type travels as: `Option<T>` as `T`, references peeled.
/// `None` for a type this check does not know.
fn type_class(ty: &str) -> Option<ColumnClass> {
    let mut t = ty.trim();
    while let Some(rest) = t.strip_prefix('&') {
        t = rest.trim_start();
        if t.starts_with('\'') {
            t = t
                .split_once(char::is_whitespace)
                .map_or("", |(_, r)| r)
                .trim_start();
        }
        t = t.strip_prefix("mut ").unwrap_or(t).trim_start();
    }
    let squeezed: String = t.split_whitespace().collect();
    if let Some(inner) = squeezed
        .strip_prefix("Option<")
        .and_then(|r| r.strip_suffix('>'))
    {
        return type_class(inner);
    }
    match squeezed.as_str() {
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize" | "f32"
        | "f64" => Some(ColumnClass::Numeric),
        "bool" => Some(ColumnClass::Boolean),
        "String" | "str" => Some(ColumnClass::Text),
        _ => None,
    }
}

/// The declared argument types of the call, and what to compare them with.
#[derive(Clone, Copy)]
struct Args<'a> {
    types: &'a [String],
    schema: &'a Schema,
    postgres: bool,
}

impl Args<'_> {
    /// `$n` meets plain column `col` of `table`: report when the two classes certainly differ.
    fn meets(&self, e: &Expr, table: &Name, col: &str, out: &mut Vec<Problem>) {
        let Expr::Param(n) = peel(e) else { return };
        let Some(ty) = usize::try_from(*n)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| self.types.get(i))
        else {
            return;
        };
        let (Some(arg), Some(c)) = (type_class(ty), self.schema.get(table, col)) else {
            return;
        };
        let col_class = c.class;
        let certain = match (arg, col_class) {
            (ColumnClass::Text, ColumnClass::Numeric | ColumnClass::Boolean)
            | (ColumnClass::Numeric | ColumnClass::Boolean, ColumnClass::Text) => true,
            (ColumnClass::Boolean, ColumnClass::Numeric)
            | (ColumnClass::Numeric, ColumnClass::Boolean) => self.postgres,
            _ => false,
        };
        if certain {
            out.push(problem(format!(
                "argument ${n} is declared `{ty}` ({}) but meets column `{col}` of `{}` ({})",
                word(arg),
                table.last(),
                word(col_class)
            )));
        }
    }
}

/// What an operand is, as far as the comparison rule can tell.
#[derive(Clone, Copy, PartialEq)]
enum Operand {
    Class(ColumnClass),
    /// A string literal: PostgreSQL gives it the type of the other operand, so it is never a fault.
    Untyped,
    Unknown,
}

fn word(c: ColumnClass) -> &'static str {
    match c {
        ColumnClass::Text => "text",
        ColumnClass::Numeric => "numeric",
        ColumnClass::Boolean => "boolean",
        _ => "other",
    }
}

/// An expression without its parentheses.
fn peel(mut e: &Expr) -> &Expr {
    while let Expr::Nested(inner) = e {
        e = inner;
    }
    e
}

/// The one base table a statement's `WHERE` is resolved against.
struct Scope<'a> {
    schema: &'a Schema,
    table: &'a Name,
    alias: Option<&'a str>,
    /// Run the PostgreSQL operand-class rule.
    postgres: bool,
    args: Option<Args<'a>>,
}

impl Scope<'_> {
    fn operand(&self, e: &Expr) -> Operand {
        match e {
            Expr::Literal(Literal::Number(_)) => Operand::Class(ColumnClass::Numeric),
            Expr::Literal(Literal::Bool(_)) => Operand::Class(ColumnClass::Boolean),
            Expr::Literal(Literal::String(_)) => Operand::Untyped,
            Expr::Column(name) => {
                let qualifier = name.0.len().checked_sub(2).map(|i| name.0[i].as_str());
                if qualifier.is_some_and(|q| Some(q) != self.alias && q != self.table.last()) {
                    return Operand::Unknown;
                }
                match self.schema.get(self.table, name.last()) {
                    Some(c)
                        if matches!(
                            c.class,
                            ColumnClass::Text | ColumnClass::Numeric | ColumnClass::Boolean
                        ) =>
                    {
                        Operand::Class(c.class)
                    }
                    _ => Operand::Unknown,
                }
            }
            _ => Operand::Unknown,
        }
    }

    /// One comparison. Reported only when at least one side is a column and both classes are known.
    fn compare(&self, l: &Expr, r: &Expr, out: &mut Vec<Problem>) {
        let (l, r) = (peel(l), peel(r));
        if let Some(args) = &self.args {
            for (param, col) in [(l, r), (r, l)] {
                if let Expr::Column(name) = col
                    && matches!(self.operand(col), Operand::Class(_))
                {
                    args.meets(param, self.table, name.last(), out);
                }
            }
        }
        if !self.postgres {
            return;
        }
        let (Operand::Class(a), Operand::Class(b)) = (self.operand(l), self.operand(r)) else {
            return;
        };
        let column = |e: &Expr| matches!(e, Expr::Column(_));
        if a != b && (column(l) || column(r)) {
            let side = |e: &Expr, c: ColumnClass| match e {
                Expr::Column(n) => format!("column `{}` ({})", n.last(), word(c)),
                _ => format!("a {} literal", word(c)),
            };
            out.push(problem(format!(
                "cannot compare {} with {}: PostgreSQL has no operator for those types",
                side(l, a),
                side(r, b)
            )));
        }
    }

    fn walk(&self, e: &Expr, out: &mut Vec<Problem>) {
        match e {
            Expr::Nested(inner) => self.walk(inner, out),
            Expr::Binary { op, left, right } => match op {
                BinaryOp::And | BinaryOp::Or => {
                    self.walk(left, out);
                    self.walk(right, out);
                }
                BinaryOp::Eq
                | BinaryOp::NotEq
                | BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::Gt
                | BinaryOp::GtEq => self.compare(left, right, out),
                _ => {}
            },
            Expr::Unary {
                op: ast::UnaryOp::Not,
                expr,
            } => self.walk(expr, out),
            Expr::Between {
                expr, low, high, ..
            } => {
                self.compare(expr, low, out);
                self.compare(expr, high, out);
            }
            Expr::InList { expr, list, .. } => {
                for item in list {
                    self.compare(expr, item, out);
                }
            }
            _ => {}
        }
    }
}

/// The operand-class check over the `WHERE` of a one-table `SELECT`, `UPDATE` or `DELETE`.
fn operand_classes(
    stmt: &ast::Statement,
    schema: &Schema,
    postgres: bool,
    args: Option<Args<'_>>,
) -> Vec<Problem> {
    let mut out = Vec::new();
    let (table, alias, where_) = match stmt {
        ast::Statement::Query(q) if q.with.is_empty() => {
            let SetExpr::Select(sel) = &q.body else {
                return out;
            };
            let [TableRef::Table { name, alias }] = &sel.from[..] else {
                return out;
            };
            (name, alias.as_deref(), sel.where_.as_ref())
        }
        ast::Statement::Update(u) if u.from.is_empty() => {
            (&u.table, u.alias.as_deref(), u.where_.as_ref())
        }
        ast::Statement::Delete(d) if d.using.is_empty() => {
            (&d.table, d.alias.as_deref(), d.where_.as_ref())
        }
        _ => return out,
    };
    if let Some(w) = where_ {
        Scope {
            schema,
            table,
            alias,
            postgres,
            args,
        }
        .walk(w, &mut out);
    }
    if let (Some(args), ast::Statement::Update(u)) = (args, stmt) {
        for a in &u.set {
            args.meets(&a.value, &u.table, &a.column, &mut out);
        }
    }
    out
}

/// `INSERT INTO t (cols) VALUES (..)`: each `$n` at position `j` meets column `j`.
fn insert_args(i: &ast::Insert, args: Args<'_>) -> Vec<Problem> {
    let mut out = Vec::new();
    if let InsertSource::Values(rows) = &i.source {
        for row in rows.iter().filter(|r| r.len() == i.columns.len()) {
            for (e, col) in row.iter().zip(&i.columns) {
                args.meets(e, &i.table, col, &mut out);
            }
        }
    }
    out
}

impl Checker for PersistantChecker {
    fn check(&self, stmt: &ast::Statement, ctx: &CheckContext<'_>) -> Vec<Problem> {
        let Some(schema) = ctx.schema else {
            return Vec::new();
        };
        let postgres = ctx.source.eq_ignore_ascii_case("postgres");
        let args = ctx.param_types.map(|types| Args {
            types,
            schema,
            postgres,
        });
        let found = operand_classes(stmt, schema, postgres, args);
        if !found.is_empty() {
            return found;
        }
        let ast::Statement::Insert(i) = stmt else {
            return Vec::new();
        };
        let mut out = args.map(|a| insert_args(i, a)).unwrap_or_default();
        let table = i.table.last();

        // A NOT NULL column with no default that the INSERT leaves out. Only when the INSERT names its
        // columns (or is DEFAULT VALUES): a positional INSERT supplies every column, and a wrong width
        // is UniSQL's check.
        let names_columns =
            !i.columns.is_empty() || matches!(i.source, InsertSource::DefaultValues);
        if names_columns && let Some(all) = schema.complete_columns(&i.table) {
            for name in all {
                if i.columns.iter().any(|c| c.eq_ignore_ascii_case(name)) {
                    continue;
                }
                if let Some(col) = schema.get(&i.table, name)
                    && col.nullable == Some(false)
                    && col.has_default == Some(false)
                {
                    out.push(problem(format!(
                        "column `{name}` of `{table}` is NOT NULL with no default, but the INSERT leaves it out"
                    )));
                }
            }
        }

        // ON CONFLICT (cols): the columns must be one of the stated unique keys.
        if let Some(oc) = &i.on_conflict
            && !oc.target.is_empty()
            && let Some(keys) = schema.unique_keys(&i.table)
            && !keys.iter().any(|k| same_set(k, &oc.target))
        {
            let have: Vec<String> = keys.iter().map(|k| format!("({})", k.join(", "))).collect();
            out.push(problem(format!(
                "ON CONFLICT ({}) is not a unique key of `{table}` (its unique keys: {})",
                oc.target.join(", "),
                have.join(", ")
            )));
        }
        out
    }
}
