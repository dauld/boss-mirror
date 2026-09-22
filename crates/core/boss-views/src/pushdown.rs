//! Splitting a filter into "what SQL can answer" and a residual.
//!
//! # The contract
//!
//! Pushdown is an **optimization and only an optimization**. Whatever
//! SQL returns, the full original predicate is re-evaluated against
//! every row in-process before it reaches the caller. So the emitted
//! condition must be **implied by** the filter — a superset the
//! residual then trims. Never a subset, and never something the
//! database refuses to run.
//!
//! - An extractor that finds nothing is correct, just slow.
//! - An extractor that finds a term is correct, just faster.
//! - An extractor that finds a term *wrongly* is the only way to be
//!   wrong. Two ways to get that wrong, both learned the hard way:
//!   emitting a **narrower** condition than the filter (rows vanish
//!   with no way to recover them), or emitting one Postgres **refuses
//!   to type** (the request 500s instead of answering).
//!
//! # Why AND and OR differ
//!
//! Under AND, dropping a branch widens: `A` alone returns a superset
//! of `A AND B`, and the residual trims it. So a conjunct pushes down
//! whether or not its sibling does.
//!
//! Under OR, dropping a branch *narrows* — rows satisfying only the
//! dropped branch are lost, and the residual cannot get them back
//! because SQL never returned them. So a disjunction pushes down only
//! if **every** branch does. Approximating the unexpressible branch as
//! TRUE is sound but collapses the whole OR to TRUE, which is the same
//! as pushing nothing.
//!
//! That asymmetry is the entire subtlety here, and it is why
//! [`Pushdown::Or`] is built only from a complete set of branches.
//!
//! # Why range bounds are pushed *loose*
//!
//! `occurred_at` is a real timestamp in SQL, but the residual compares
//! the row's **rendered RFC3339 string** lexicographically, because
//! that is what the JSON row carries. Those two orderings agree for
//! well-formed same-offset timestamps and diverge at the edges — an
//! unpadded month (`2026-1-05`), a literal that is a prefix of the
//! rendered value (`2026-08-05` against
//! `2026-08-05T00:00:00+00:00`), a differing offset.
//!
//! The relaxation is **inclusivity only**: `>` is pushed as `>=` and
//! `<` as `<=`, at the exact parsed instant. That is enough, because
//! the row side is always padded RFC3339 (`to_rfc3339` on a UTC
//! instant), and lexicographic order on padded ISO-8601 IS
//! chronological order. A malformed literal — an unpadded month like
//! `2025-4-01` — makes the *residual* the stricter of the two, which
//! is the safe direction: SQL returns rows, the residual drops them.
//!
//! An earlier version also widened the bound by a day, on the theory
//! that more slack is safer. It was not. The scan is
//! `ORDER BY audit_id DESC LIMIT n`, so widening the window moves the
//! rows the limit keeps: a one-day query became a three-day window
//! whose newest `n` rows were **entirely outside the day asked for**,
//! and a filter matching 5,525 events answered 0. Slack is not free
//! when a LIMIT sits on top of it — the bound must be as tight as
//! soundness allows, not as loose as it can get away with.
//!
//! A literal that does not parse as a date or timestamp is not pushed
//! at all — `occurred_at > "banana"` would make Postgres reject the
//! statement, which is the 500 class again.
//!
//! # Why NOT is never pushed
//!
//! Negation inverts the direction of approximation: if `A'` is a
//! superset of `A`, then `NOT A'` is a *subset* of `NOT A`. Pushing a
//! negated term is therefore sound only when the inner condition is
//! **exact**, and this extractor cannot promise exactness — the
//! in-process evaluator has its own semantics for missing fields and
//! type mismatches that need not agree with SQL's for every value. So
//! `NOT` stops the walk. Widening this needs an exactness proof, not
//! just a new match arm.

use boss_expr::{BinaryOp, Expr, UnaryOp, Value};
use chrono::{DateTime, NaiveDate, Utc};

/// What a pushable column holds, so a literal is only pushed where the
/// database can compare it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Text,
    Timestamp,
    /// A JSONB column. A filter reaches inside it with a dotted path
    /// (`payload.sku`), which `#>>` extracts as text.
    Json,
}

/// A value bound into the emitted statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Bound {
    Text(String),
    TextList(Vec<String>),
    Timestamp(DateTime<Utc>),
}

/// A condition SQL can serve, as a tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Pushdown {
    /// `<column> = <text literal>`.
    Eq {
        /// A projection column, resolved through the source's declared
        /// mapping — never caller text.
        column: &'static str,
        value: String,
    },
    /// `<column> IS DISTINCT FROM <text literal>`.
    ///
    /// Not `<>`. The residual treats a missing field as *different
    /// from* any string — `values_equal(Null, String)` is false, so
    /// `!=` evaluates true — while SQL's `<>` yields NULL against a
    /// NULL column and drops the row. `IS DISTINCT FROM` is the
    /// operator that agrees with the residual; `<>` would be the
    /// stricter of the two and lose rows the filter wants.
    Neq {
        column: &'static str,
        value: String,
    },
    /// `<column> #>> <path> = <text literal>` — a term reaching into
    /// a JSONB column.
    ///
    /// `#>>` yields TEXT, while the residual compares the JSON value
    /// at its own type. Where they differ — a numeric `5` in the
    /// payload against a `"5"` literal — SQL matches and the residual
    /// rejects, so SQL is the looser of the two. That is the safe
    /// direction, and it is why this is not exact and cannot be
    /// negated.
    JsonEq {
        column: &'static str,
        path: Vec<String>,
        value: String,
        /// `IS DISTINCT FROM` rather than `=`, for the same NULL
        /// reason as [`Pushdown::Neq`]: a missing path extracts NULL,
        /// and the residual counts that as different from any string.
        negated: bool,
    },
    /// `<column> = ANY(<array>)` — a set membership.
    ///
    /// The DSL has no `IN` keyword, so this is never parsed directly:
    /// it is a collapse of `a = "x" OR a = "y" OR …` on one column,
    /// which is how a filter author spells set membership. Same rows
    /// as the OR it replaces, one bind instead of N.
    In {
        column: &'static str,
        values: Vec<String>,
    },
    /// `<column> >= <ts>` or `<column> <= <ts>`. Only these two
    /// operators: strict bounds are relaxed to inclusive ones so the
    /// pushed condition can never be narrower than the residual's.
    Range {
        column: &'static str,
        /// True for `>=`, false for `<=`.
        lower: bool,
        value: DateTime<Utc>,
    },
    All(Vec<Pushdown>),
    Any(Vec<Pushdown>),
}

impl Pushdown {
    /// Number of leaf comparisons — what `pushed_down` reports.
    pub fn term_count(&self) -> usize {
        match self {
            Pushdown::Eq { .. }
            | Pushdown::Neq { .. }
            | Pushdown::JsonEq { .. }
            | Pushdown::Range { .. } => 1,
            // One term per value: `pushed_down` counts what the filter
            // author wrote, and they wrote N equalities.
            Pushdown::In { values, .. } => values.len(),
            Pushdown::All(cs) | Pushdown::Any(cs) => cs.iter().map(Self::term_count).sum(),
        }
    }

    /// Render to SQL, appending binds in the order they appear.
    ///
    /// `next_param` is the 1-based index of the next free placeholder,
    /// advanced as terms are emitted.
    pub fn to_sql(&self, next_param: &mut usize, binds: &mut Vec<Bound>) -> String {
        match self {
            Pushdown::Eq { column, value } => {
                binds.push(Bound::Text(value.clone()));
                let s = format!("{column} = ${next_param}");
                *next_param += 1;
                s
            }
            Pushdown::Neq { column, value } => {
                binds.push(Bound::Text(value.clone()));
                let s = format!("{column} IS DISTINCT FROM ${next_param}");
                *next_param += 1;
                s
            }
            Pushdown::JsonEq {
                column,
                path,
                value,
                negated,
            } => {
                binds.push(Bound::TextList(path.clone()));
                let path_param = *next_param;
                *next_param += 1;
                binds.push(Bound::Text(value.clone()));
                let op = if *negated { "IS DISTINCT FROM" } else { "=" };
                let s = format!("{column} #>> ${path_param}::text[] {op} ${next_param}");
                *next_param += 1;
                s
            }
            Pushdown::In { column, values } => {
                binds.push(Bound::TextList(values.clone()));
                let s = format!("{column} = ANY(${next_param})");
                *next_param += 1;
                s
            }
            Pushdown::Range {
                column,
                lower,
                value,
            } => {
                binds.push(Bound::Timestamp(*value));
                let op = if *lower { ">=" } else { "<=" };
                let s = format!("{column} {op} ${next_param}");
                *next_param += 1;
                s
            }
            Pushdown::All(cs) => Self::join(cs, " AND ", next_param, binds),
            Pushdown::Any(cs) => Self::join(cs, " OR ", next_param, binds),
        }
    }

    fn join(
        parts: &[Pushdown],
        sep: &str,
        next_param: &mut usize,
        binds: &mut Vec<Bound>,
    ) -> String {
        let rendered: Vec<String> = parts.iter().map(|p| p.to_sql(next_param, binds)).collect();
        // Always parenthesised: an unparenthesised OR inside an AND
        // would bind wrong and silently widen the result.
        format!("({})", rendered.join(sep))
    }
}

/// Columns a source lets a filter push into SQL.
///
/// Maps the identifier a filter writer types to a column this crate
/// owns. Both halves are compile-time constants, which is what keeps
/// operator text out of the statement: a filter selects among these,
/// it can never name a new one.
///
/// Each entry is `(filter name, column, column type)`. The type is not
/// decoration: a literal is only pushed where the database can compare
/// it. Binding a text value against a `uuid` column makes Postgres
/// refuse the statement — `operator does not exist: uuid = text` —
/// turning a filter that used to answer into a 500.
pub type PushableColumns = &'static [(&'static str, &'static str, ColumnType)];

/// Parse a filter literal as an instant.
///
/// Accepts a full RFC3339 timestamp or a plain `YYYY-MM-DD` date.
/// Anything else is not pushed — a literal Postgres cannot cast would
/// fail the statement rather than the row.
fn parse_instant(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc())
}

/// Extract the largest condition SQL can answer, or `None`.
pub fn extract(expr: &Expr, pushable: PushableColumns) -> Option<Pushdown> {
    match expr {
        // AND: either branch alone still widens, so take what we can.
        Expr::BinaryOp(BinaryOp::And, lhs, rhs) => {
            match (extract(lhs, pushable), extract(rhs, pushable)) {
                (Some(a), Some(b)) => Some(Pushdown::All(vec![a, b])),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }
        // OR: all-or-nothing. A missing branch would narrow.
        Expr::BinaryOp(BinaryOp::Or, lhs, rhs) => {
            match (extract(lhs, pushable), extract(rhs, pushable)) {
                (Some(a), Some(b)) => Some(collapse_any(a, b)),
                _ => None,
            }
        }
        Expr::BinaryOp(BinaryOp::Eq, lhs, rhs) => as_eq(lhs, rhs, pushable),
        Expr::BinaryOp(BinaryOp::Neq, lhs, rhs) => as_eq(lhs, rhs, pushable).map(|p| match p {
            Pushdown::Eq { column, value } => Pushdown::Neq { column, value },
            Pushdown::JsonEq {
                column,
                path,
                value,
                ..
            } => Pushdown::JsonEq {
                column,
                path,
                value,
                negated: true,
            },
            other => other,
        }),
        Expr::BinaryOp(
            op @ (BinaryOp::Lt | BinaryOp::Lte | BinaryOp::Gt | BinaryOp::Gte),
            lhs,
            rhs,
        ) => as_range(*op, lhs, rhs, pushable),
        // NOT, function calls, bare terms: residual. Every BinaryOp
        // variant is handled above, so there is deliberately no
        // catch-all for them here — adding a new operator to the DSL
        // should fail this match rather than silently land in the
        // residual, where it would cost a full scan nobody noticed.
        // A list literal (4d53fae2) is residual for the same reason:
        // it exists so a rule file can pass a handler an argument
        // list, and no comparison over one is expressible in SQL here
        // — `= ANY(...)` is what `collapse_any` builds out of an OR
        // chain, not a filter term an author writes.
        Expr::UnaryOp(UnaryOp::Not, _)
        | Expr::FunctionCall(_, _)
        | Expr::Identifier(_)
        | Expr::Literal(_)
        | Expr::List(_) => None,
    }
}

/// Fold two OR branches, collapsing same-column equalities into a
/// single `= ANY(...)`.
///
/// Purely a rewrite: `a = "x" OR a = "y"` and `a = ANY(['x','y'])`
/// select the same rows. It exists because set membership is what
/// filter authors are actually expressing when they chain equalities,
/// and saying so in one term reads better in a plan than an OR chain.
fn collapse_any(a: Pushdown, b: Pushdown) -> Pushdown {
    // Only plain column equalities collapse into `= ANY(...)`. A
    // JsonEq carries its own path, so two of them on the same column
    // are not a set on one value.
    match (a, b) {
        (
            Pushdown::Eq {
                column: ca,
                value: va,
            },
            Pushdown::Eq {
                column: cb,
                value: vb,
            },
        ) if ca == cb => Pushdown::In {
            column: ca,
            values: vec![va, vb],
        },
        // Left-associated chains arrive as In-then-Eq.
        (
            Pushdown::In {
                column: ca,
                mut values,
            },
            Pushdown::Eq {
                column: cb,
                value: vb,
            },
        ) if ca == cb => {
            values.push(vb);
            Pushdown::In { column: ca, values }
        }
        (
            Pushdown::Eq {
                column: ca,
                value: va,
            },
            Pushdown::In {
                column: cb,
                mut values,
            },
        ) if ca == cb => {
            values.insert(0, va);
            Pushdown::In { column: ca, values }
        }
        (x, y) => Pushdown::Any(vec![x, y]),
    }
}

/// `<identifier> = <string literal>` in either order, where the
/// identifier names a pushable column.
fn as_eq(lhs: &Expr, rhs: &Expr, pushable: PushableColumns) -> Option<Pushdown> {
    let (path, value) = match (lhs, rhs) {
        (Expr::Identifier(p), Expr::Literal(v)) => (p, v),
        (Expr::Literal(v), Expr::Identifier(p)) => (p, v),
        _ => return None,
    };
    // A dotted path reaches inside a JSONB column, if the head names
    // one. `payload.sku` becomes `payload #>> '{sku}'`.
    if path.len() > 1 {
        let (head, rest) = path.split_first()?;
        let column = pushable
            .iter()
            .find(|(filter_name, _, ty)| filter_name == head && *ty == ColumnType::Json)
            .map(|(_, col, _)| *col)?;
        let Value::String(s) = value else {
            return None;
        };
        return Some(Pushdown::JsonEq {
            column,
            path: rest.to_vec(),
            value: s.clone(),
            negated: false,
        });
    }
    let [name] = path.as_slice() else {
        return None;
    };
    // Text literals only. Every pushable column is TEXT; binding an
    // int or a bool against one makes Postgres reject the statement
    // rather than the row, so a filter that used to answer 0 would
    // instead fail the request.
    let Value::String(s) = value else {
        return None;
    };
    // Text columns only. Equality against a timestamp column would
    // need the same parse-and-widen care a range gets, and an exact
    // instant is not a question anyone asks of an event log.
    let column = pushable
        .iter()
        .find(|(filter_name, _, ty)| filter_name == name && *ty == ColumnType::Text)
        .map(|(_, col, _)| *col)?;
    Some(Pushdown::Eq {
        column,
        value: s.clone(),
    })
}

/// `<identifier> <op> <literal>` against a timestamp column.
///
/// The emitted bound is relaxed in exactly one way: strict operators
/// become inclusive. The instant itself is NOT moved — see the module
/// docs on why extra slack breaks a scan that carries a LIMIT.
fn as_range(op: BinaryOp, lhs: &Expr, rhs: &Expr, pushable: PushableColumns) -> Option<Pushdown> {
    // Normalise to `<identifier> <op> <literal>`, flipping the
    // operator if the operands are reversed — `"x" < occurred_at` is a
    // lower bound on the column, not an upper one.
    let (path, value, op) = match (lhs, rhs) {
        (Expr::Identifier(p), Expr::Literal(v)) => (p, v, op),
        (Expr::Literal(v), Expr::Identifier(p)) => (p, v, flip(op)),
        _ => return None,
    };
    let [name] = path.as_slice() else {
        return None;
    };
    let Value::String(s) = value else {
        return None;
    };
    let column = pushable
        .iter()
        .find(|(filter_name, _, ty)| filter_name == name && *ty == ColumnType::Timestamp)
        .map(|(_, col, _)| *col)?;
    let instant = parse_instant(s)?;
    Some(Pushdown::Range {
        column,
        lower: matches!(op, BinaryOp::Gt | BinaryOp::Gte),
        value: instant,
    })
}

/// The operator as seen from the column's side.
fn flip(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Lt => BinaryOp::Gt,
        BinaryOp::Lte => BinaryOp::Gte,
        BinaryOp::Gt => BinaryOp::Lt,
        BinaryOp::Gte => BinaryOp::Lte,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT_COLUMNS: PushableColumns = &[
        ("payload", "payload", ColumnType::Json),
        ("kind", "kind", ColumnType::Text),
        ("source", "source", ColumnType::Text),
        ("subject_kind", "subject_kind", ColumnType::Text),
        ("subject_id", "subject_id", ColumnType::Text),
        ("timestamp", "occurred_at", ColumnType::Timestamp),
    ];

    fn ex(s: &str) -> Option<Pushdown> {
        extract(&boss_expr::parse(s).expect("parses"), EVENT_COLUMNS)
    }

    fn sql_of(s: &str) -> (String, Vec<Bound>) {
        let p = ex(s).expect("pushes down");
        let mut n = 1;
        let mut binds = Vec::new();
        let out = p.to_sql(&mut n, &mut binds);
        (out, binds)
    }

    #[test]
    fn a_bare_equality_pushes_down() {
        let (sql, binds) = sql_of("kind = \"products.consumed\"");
        assert_eq!(sql, "kind = $1");
        assert_eq!(binds, vec![Bound::Text("products.consumed".into())]);
    }

    #[test]
    fn literal_on_the_left_works_too() {
        let (sql, _) = sql_of("\"products.consumed\" = kind");
        assert_eq!(sql, "kind = $1");
    }

    #[test]
    fn an_or_of_two_pushable_branches_now_pushes() {
        // The case this extension exists for: previously it pushed
        // nothing, fell back to a capped scan, and reported 0 against
        // a true 16. Same-column equalities then collapse to a set —
        // see the dedicated test below; asserting rows-selected rather
        // than exact SQL keeps this one about the OR itself.
        assert!(ex("kind = \"a\" OR kind = \"b\"").is_some());
        assert_eq!(ex("kind = \"a\" OR kind = \"b\"").unwrap().term_count(), 2);
    }

    #[test]
    fn an_or_with_one_unpushable_branch_pushes_nothing() {
        // THE soundness case. Pushing `kind = "a"` alone would drop
        // every row that matched only on the payload term, and the
        // residual could never recover them — SQL never returned them.
        assert_eq!(ex("kind = \"a\" OR amount = \"5\""), None);
        assert_eq!(ex("amount = \"5\" OR kind = \"a\""), None);
    }

    #[test]
    fn an_and_with_one_unpushable_branch_still_pushes_the_other() {
        // The dual: dropping a conjunct widens, which is safe.
        let (sql, binds) = sql_of("kind = \"a\" AND amount = \"5\"");
        assert_eq!(sql, "kind = $1");
        assert_eq!(binds, vec![Bound::Text("a".into())]);
    }

    #[test]
    fn an_or_nested_under_an_and_pushes_as_a_group() {
        let (sql, binds) = sql_of("subject_kind = \"account\" AND (kind = \"a\" OR kind = \"b\")");
        // The same-column OR collapses to a set; the AND still binds
        // both halves.
        assert_eq!(sql, "(subject_kind = $1 AND kind = ANY($2))");
        assert_eq!(binds.len(), 2);
    }

    #[test]
    fn an_unpushable_branch_inside_a_nested_or_drops_only_that_or() {
        // The AND keeps its pushable half; the OR contributes nothing.
        let (sql, binds) =
            sql_of("subject_kind = \"account\" AND (kind = \"a\" OR amount = \"5\")");
        assert_eq!(sql, "subject_kind = $1");
        assert_eq!(binds, vec![Bound::Text("account".into())]);
    }

    #[test]
    fn or_groups_are_parenthesised_inside_an_and() {
        // Unparenthesised, `a AND b OR c` binds as `(a AND b) OR c`
        // and silently returns rows the filter excludes.
        // Two DIFFERENT columns, so the disjunction survives rather
        // than collapsing to a set — the parenthesisation is the point.
        let (sql, _) = sql_of("kind = \"a\" AND (subject_id = \"s\" OR source = \"x\")");
        assert!(
            sql.contains("(subject_id = $2 OR source = $3)"),
            "got: {sql}"
        );
    }

    #[test]
    fn not_is_never_pushed_even_when_its_inner_term_is_pushable() {
        // Negation inverts the direction of approximation; see the
        // module docs. Sound only with an exactness proof this
        // extractor does not have.
        assert_eq!(ex("NOT kind = \"a\""), None);
        assert_eq!(ex("NOT (kind = \"a\" OR kind = \"b\")"), None);
    }

    #[test]
    fn a_non_text_literal_never_pushes() {
        // Regression: these reached Postgres as `text = bigint` and
        // `uuid = text` and 500'd the request. Before pushdown existed
        // they evaluated in-process and returned nothing, gracefully.
        assert_eq!(ex("kind = 5"), None);
        assert_eq!(ex("kind = true"), None);
        assert_eq!(ex("subject_id = null"), None);
    }

    #[test]
    fn an_unmapped_field_does_not_push() {
        assert_eq!(ex("amount = \"5\""), None);
        // A dotted path whose head is not a declared JSON column has
        // no column behind it either.
        assert_eq!(ex("nosuch.total = \"5\""), None);
    }

    #[test]
    fn a_payload_path_pushes_as_a_json_extraction() {
        // The case this exists for: without it a payload filter pushed
        // nothing, the scan took the newest N rows, and the count came
        // back 0 against a true 351.
        let (sql, binds) = sql_of("payload.sku = \"KEG-HALF\"");
        assert_eq!(sql, "payload #>> $1::text[] = $2");
        assert_eq!(
            binds,
            vec![
                Bound::TextList(vec!["sku".into()]),
                Bound::Text("KEG-HALF".into())
            ]
        );
    }

    #[test]
    fn a_deep_payload_path_keeps_every_segment() {
        let (_, binds) = sql_of("payload.subject.id = \"acc-1\"");
        assert_eq!(
            binds[0],
            Bound::TextList(vec!["subject".into(), "id".into()])
        );
    }

    #[test]
    fn a_negated_payload_path_uses_is_distinct_from() {
        // Same NULL reason as plain `!=`: a missing path extracts NULL,
        // and the residual counts that as different from any string.
        let (sql, _) = sql_of("payload.sku != \"KEG-HALF\"");
        assert_eq!(sql, "payload #>> $1::text[] IS DISTINCT FROM $2");
    }

    #[test]
    fn a_non_text_literal_against_a_payload_path_does_not_push() {
        // `#>>` yields text; binding an int would compare text to
        // bigint and fail the statement.
        assert_eq!(ex("payload.qty = 4"), None);
    }

    #[test]
    fn payload_paths_do_not_collapse_into_a_set() {
        // Two JsonEq on the same column are not a set on one value —
        // they carry different paths.
        let (sql, _) = sql_of("payload.sku = \"a\" OR payload.other = \"b\"");
        assert!(sql.contains(" OR "), "must stay a disjunction, got: {sql}");
    }

    #[test]
    fn a_payload_path_combines_with_a_column_term() {
        let (sql, binds) = sql_of("kind = \"products.consumed\" AND payload.sku = \"KEG\"");
        assert_eq!(sql, "(kind = $1 AND payload #>> $2::text[] = $3)");
        assert_eq!(binds.len(), 3);
    }

    #[test]
    fn an_ordering_comparison_on_a_text_column_does_not_push() {
        // Ranges are for timestamps; a lexicographic bound on `kind`
        // is not a question the index can answer usefully.
        assert_eq!(ex("kind > \"a\""), None);
        assert_eq!(ex("kind <= \"a\""), None);
    }

    #[test]
    fn neq_pushes_as_is_distinct_from_not_as_not_equals() {
        // THE soundness case for `!=`. The residual treats a missing
        // field as different from any string —
        // `values_equal(Null, String)` is false, so `!=` evaluates
        // true. SQL's `<>` yields NULL against a NULL column and drops
        // the row, which would make SQL the stricter of the two and
        // lose rows the filter wants. `IS DISTINCT FROM` agrees.
        let (sql, binds) = sql_of("subject_id != \"acc-1\"");
        assert_eq!(sql, "subject_id IS DISTINCT FROM $1");
        assert_eq!(binds, vec![Bound::Text("acc-1".into())]);
        assert!(!sql.contains("<>"), "`<>` would drop NULL rows");
    }

    #[test]
    fn neq_respects_the_column_type_like_equality_does() {
        assert_eq!(ex("kind != 5"), None);
        assert_eq!(ex("timestamp != \"2025-01-01\""), None);
    }

    #[test]
    fn an_or_of_equalities_on_one_column_collapses_to_any() {
        // The DSL has no IN keyword, so this is how a filter author
        // spells set membership. Same rows as the OR chain, one bind.
        let (sql, binds) = sql_of("kind = \"a\" OR kind = \"b\"");
        assert_eq!(sql, "kind = ANY($1)");
        assert_eq!(binds, vec![Bound::TextList(vec!["a".into(), "b".into()])]);
    }

    #[test]
    fn a_longer_chain_collapses_into_one_set() {
        let (sql, binds) = sql_of("kind = \"a\" OR kind = \"b\" OR kind = \"c\"");
        assert_eq!(sql, "kind = ANY($1)");
        assert_eq!(
            binds,
            vec![Bound::TextList(vec!["a".into(), "b".into(), "c".into()])]
        );
    }

    #[test]
    fn an_or_across_different_columns_stays_a_disjunction() {
        // Collapsing these would be wrong — they are not a set on one
        // column.
        let (sql, _) = sql_of("kind = \"a\" OR subject_id = \"s\"");
        assert_eq!(sql, "(kind = $1 OR subject_id = $2)");
    }

    #[test]
    fn a_collapsed_set_still_counts_every_term_it_replaced() {
        // pushed_down reports what the author wrote, not how many
        // binds it took.
        assert_eq!(
            ex("kind = \"a\" OR kind = \"b\" OR kind = \"c\"")
                .unwrap()
                .term_count(),
            3
        );
    }

    fn range_of(s: &str) -> (bool, DateTime<Utc>) {
        match ex(s).expect("pushes down") {
            Pushdown::Range { lower, value, .. } => (lower, value),
            other => panic!("expected a range, got {other:?}"),
        }
    }

    #[test]
    fn a_strict_lower_bound_is_relaxed_to_inclusive_but_not_moved() {
        // `>` becomes `>=` — that is the whole relaxation. Moving the
        // instant outward as well made a bounded window return rows
        // exclusively from OUTSIDE it, because the LIMIT keeps the
        // newest rows of whatever window it is given.
        let (lower, value) = range_of("timestamp > \"2026-08-05\"");
        assert!(lower);
        assert_eq!(value.to_rfc3339(), "2026-08-05T00:00:00+00:00");
    }

    #[test]
    fn an_upper_bound_is_inclusive_and_not_moved() {
        let (lower, value) = range_of("timestamp <= \"2026-08-05\"");
        assert!(!lower);
        assert_eq!(value.to_rfc3339(), "2026-08-05T00:00:00+00:00");
    }

    #[test]
    fn a_bounded_window_pushes_the_exact_edges_not_widened_ones() {
        // Regression. Widening these by a day turned a one-day query
        // into a three-day window, and `ORDER BY audit_id DESC LIMIT n`
        // then kept only the newest rows — all of them outside the day
        // asked for. A filter matching 5,525 events answered 0.
        let lo = range_of("timestamp >= \"2025-04-01\"");
        let hi = range_of("timestamp <= \"2025-04-02\"");
        assert_eq!(lo.1.to_rfc3339(), "2025-04-01T00:00:00+00:00");
        assert_eq!(hi.1.to_rfc3339(), "2025-04-02T00:00:00+00:00");
    }

    #[test]
    fn a_reversed_range_flips_the_operator() {
        // `"x" < timestamp` is a LOWER bound on the column. Reading it
        // as an upper bound would exclude everything the filter wants.
        let (lower, _) = range_of("\"2026-08-05\" < timestamp");
        assert!(lower, "literal-on-the-left must flip to a lower bound");
        let (lower, _) = range_of("\"2026-08-05\" > timestamp");
        assert!(!lower);
    }

    #[test]
    fn a_full_rfc3339_literal_parses() {
        let (lower, value) = range_of("timestamp >= \"2026-08-05T12:30:00+00:00\"");
        assert!(lower);
        assert_eq!(value.to_rfc3339(), "2026-08-05T12:30:00+00:00");
    }

    #[test]
    fn an_unparseable_instant_is_not_pushed() {
        // Would reach Postgres as `invalid input syntax for type
        // timestamp with time zone` — the 500 class.
        assert_eq!(ex("timestamp > \"banana\""), None);
        assert_eq!(ex("timestamp > \"2026-13-45\""), None);
        assert_eq!(ex("timestamp > 5"), None);
    }

    #[test]
    fn a_range_combines_with_equality_under_and() {
        let (sql, binds) = sql_of("kind = \"a\" AND timestamp > \"2026-08-05\"");
        assert_eq!(sql, "(kind = $1 AND occurred_at >= $2)");
        assert_eq!(binds.len(), 2);
        assert!(matches!(binds[1], Bound::Timestamp(_)));
    }

    #[test]
    fn a_bounded_window_pushes_both_ends() {
        let (sql, _) = sql_of("timestamp >= \"2026-01-01\" AND timestamp <= \"2026-02-01\"");
        assert_eq!(sql, "(occurred_at >= $1 AND occurred_at <= $2)");
    }

    #[test]
    fn an_or_of_ranges_pushes_as_a_group() {
        let (sql, _) = sql_of("timestamp < \"2026-01-01\" OR timestamp > \"2026-08-01\"");
        assert_eq!(sql, "(occurred_at <= $1 OR occurred_at >= $2)");
    }

    #[test]
    fn term_count_reports_leaves_not_nodes() {
        assert_eq!(ex("kind = \"a\"").unwrap().term_count(), 1);
        assert_eq!(ex("kind = \"a\" OR kind = \"b\"").unwrap().term_count(), 2);
        assert_eq!(
            ex("subject_kind = \"x\" AND (kind = \"a\" OR kind = \"b\")")
                .unwrap()
                .term_count(),
            3
        );
    }

    #[test]
    fn placeholders_are_numbered_in_bind_order() {
        // Off-by-one here swaps two values between columns, which is a
        // wrong answer rather than a slow one.
        let (sql, binds) =
            sql_of("kind = \"k\" AND (subject_kind = \"sk\" OR subject_id = \"si\")");
        assert_eq!(
            sql,
            "(kind = $1 AND (subject_kind = $2 OR subject_id = $3))"
        );
        assert_eq!(
            binds,
            vec![
                Bound::Text("k".into()),
                Bound::Text("sk".into()),
                Bound::Text("si".into())
            ]
        );
    }

    #[test]
    fn a_list_literal_is_residual_and_does_not_narrow_its_conjunct() {
        // The DSL gained list literals for rule ARGS (4d53fae2), and a
        // View filter shares the evaluator. SQL cannot answer one, so
        // it must fall to the residual — and, critically, a conjunct
        // beside it must still push: dropping the pushable half would
        // cost a scan, and pushing the unpushable half would narrow.
        assert!(
            ex("kind = [\"a\", \"b\"]").is_none(),
            "a comparison against a list is not expressible here"
        );
        let (sql, _) = sql_of("kind = \"k\" AND source = [\"a\"]");
        assert_eq!(sql, "kind = $1", "the pushable conjunct still pushes");
    }
}
