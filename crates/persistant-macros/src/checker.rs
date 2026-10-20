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
//! Not checked, and said so in the macro's documentation: operand classes in comparisons, the types of
//! bound arguments and of selected columns (stage 6), anything about views, CTEs or other schemas.

use unisql::ast::{self, InsertSource};
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

impl Checker for PersistantChecker {
    fn check(&self, stmt: &ast::Statement, ctx: &CheckContext<'_>) -> Vec<Problem> {
        let (Some(schema), ast::Statement::Insert(i)) = (ctx.schema, stmt) else {
            return Vec::new();
        };
        let mut out = Vec::new();
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
