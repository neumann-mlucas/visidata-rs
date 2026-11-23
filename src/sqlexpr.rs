//! SQL expressions (`:derive`, `:select`, `:filter-expr`, `g=`; S-21) and
//! how they differ between engines. The app and the Python export parse them
//! with Polars ([`for_polars`]); the SQL export hands them to DuckDB
//! ([`for_duckdb`]). Both rewrites keep VisiData's results.

use polars::prelude::{DataType, Expr, PolarsResult, Schema};
use sqlparser::ast::{BinaryOperator as B, CastKind, DataType as T, Expr as E, UnaryOperator as U};
use std::ops::ControlFlow;

/// Polars SQL functions that return an unsigned count.
const UNSIGNED: [&str; 7] = [
    "length",
    "char_length",
    "character_length",
    "octet_length",
    "bit_length",
    "strpos",
    "array_length",
];

/// `expr` as a Polars expression ([`for_polars`] first).
pub fn parse(expr: &str) -> PolarsResult<Expr> {
    polars::sql::sql_expr(for_polars(expr))
}

/// `expr` for Polars: a function that counts (`length`, `strpos`, …) gives
/// a signed number, so `length(x) - 12` goes below 0 as in VisiData, not
/// round to 4294967288. Unchanged when nothing needs it.
pub fn for_polars(expr: &str) -> String {
    rewrite(expr, |e| {
        let E::Function(f) = e else { return false };
        let name = f.name.to_string().to_lowercase();
        if !UNSIGNED.contains(&name.as_str()) {
            return false;
        }
        *e = bigint(e.clone());
        true
    })
}

/// `expr` for DuckDB: a Boolean column in arithmetic counts as 0 or 1, as in
/// Polars and VisiData's Python; DuckDB refuses `BOOLEAN - BIGINT`.
/// `schema`: the columns the expression reads. Unchanged when nothing needs it.
pub fn for_duckdb(expr: &str, schema: &Schema) -> String {
    rewrite(expr, |e| {
        let mut changed = false;
        let mut num = |e: &mut E| {
            let E::Identifier(i) = e else { return };
            if schema.get(i.value.as_str()) == Some(&DataType::Boolean) {
                *e = bigint(e.clone());
                changed = true;
            }
        };
        match e {
            E::BinaryOp {
                left,
                op: B::Plus | B::Minus | B::Multiply | B::Divide | B::Modulo,
                right,
            } => {
                num(left);
                num(right);
            }
            E::UnaryOp {
                op: U::Minus | U::Plus,
                expr,
            } => num(expr),
            _ => {}
        }
        changed
    })
}

fn bigint(e: E) -> E {
    E::Cast {
        kind: CastKind::Cast,
        expr: Box::new(e),
        data_type: T::BigInt(None),
        array: false,
        format: None,
    }
}

/// `expr` with `f` applied to each node, children first; `f` says whether
/// it changed the node. The text as written when nothing changed or it
/// doesn't parse (Polars then reports the error).
fn rewrite(expr: &str, mut f: impl FnMut(&mut E) -> bool) -> String {
    use sqlparser::{dialect::GenericDialect, parser::Parser};
    let parsed = Parser::new(&GenericDialect) // as `polars::sql::sql_expr`
        .try_with_sql(expr)
        .and_then(|mut p| p.parse_expr());
    let Ok(mut ast) = parsed else {
        return expr.to_string();
    };
    let mut changed = false;
    let _ = sqlparser::ast::visit_expressions_mut(&mut ast, |e| {
        changed |= f(e);
        ControlFlow::<()>::Continue(())
    });
    match changed {
        true => ast.to_string(),
        false => expr.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polars_counts_are_signed() {
        assert_eq!(
            for_polars("length(x) - 12"),
            "CAST(length(x) AS BIGINT) - 12"
        );
        assert_eq!(
            for_polars("STRPOS(x, 'a')"),
            "CAST(STRPOS(x, 'a') AS BIGINT)"
        );
        assert_eq!(for_polars("upper(x)  || 'a'"), "upper(x)  || 'a'");
        let df = polars::df!("x" => ["NULL"]).unwrap();
        let out = polars::prelude::IntoLazy::lazy(df)
            .select([parse("length(x) - 12").unwrap()])
            .collect()
            .unwrap();
        assert_eq!(out.columns()[0].get(0).unwrap().extract::<i64>(), Some(-8));
    }

    /// DuckDB has no `BOOLEAN - BIGINT`: a Boolean column in arithmetic is
    /// cast, as Polars reads it (0 or 1); other expressions stay as written.
    #[test]
    fn duckdb_casts_booleans_in_arithmetic() {
        let schema = Schema::from_iter([
            ("f".into(), DataType::Boolean),
            ("n".into(), DataType::Int64),
        ]);
        let e = |x: &str| for_duckdb(x, &schema);
        assert_eq!(e("f - 1"), "CAST(f AS BIGINT) - 1");
        assert_eq!(e("-\"f\" * n"), "-CAST(\"f\" AS BIGINT) * n");
        assert_eq!(e("f AND  n > 1"), "f AND  n > 1");
        assert_eq!(e("n  - 1"), "n  - 1");
    }
}
