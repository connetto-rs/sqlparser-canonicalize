//! What a predicate means, written independently of the canonicalizer.
//!
//! Two expressions with the same meaning text are the same predicate under every equivalence
//! the canonicalizer promises. Those are redundant parentheses, the dialect's name folding, the
//! order of operands and items it treats as unordered, `a > b` for `b < a`, and synonyms such
//! as `SOME` for `ANY`, `x::T` for `CAST(x AS T)`, SQLite's `a IS b` for `a IS NOT DISTINCT
//! FROM b`, a missing `ELSE` for `ELSE NULL`, a table alias with or without `AS`, a subquery
//! table with or without `*`, and a subquery without `WHERE` for one filtered by `TRUE` where
//! `TRUE` is reserved. Anything else keeps its structure.

use sqlparser::ast::{
    AccessExpr, BinaryOperator, CastKind, CeilFloorKind, Expr, FunctionArg, FunctionArgExpr,
    FunctionArguments, GroupByExpr, Ident, InTarget, ObjectName, Query, SelectItem, SetExpr,
    Subscript, TableFactor, TableInheritance, UnaryOperator, Value,
};
use sqlparser::dialect::{AnsiDialect, Dialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fold {
    Lower,
    Upper,
    Insensitive,
    Exact,
}

/// How a dialect folds column names and table qualifiers, and whether a bare `TRUE` is always
/// the boolean.
#[derive(Clone, Copy, Debug)]
pub struct Folding {
    pub column: Fold,
    pub qualifier: Fold,
    pub true_is_reserved: bool,
    pub plus_is_numeric: bool,
    pub collation_is_symmetric: bool,
}

impl Folding {
    pub fn of(dialect: &dyn Dialect) -> Self {
        let (column, qualifier) = if dialect.is::<PostgreSqlDialect>() {
            (Fold::Lower, Fold::Lower)
        } else if dialect.is::<AnsiDialect>() {
            (Fold::Upper, Fold::Upper)
        } else if dialect.is::<MySqlDialect>() {
            (Fold::Insensitive, Fold::Exact)
        } else if dialect.is::<SQLiteDialect>() {
            (Fold::Insensitive, Fold::Insensitive)
        } else {
            (Fold::Exact, Fold::Exact)
        };
        // SQLite reads a bare `TRUE` as a column named `true` when the table has one.
        let true_is_reserved = dialect.is::<PostgreSqlDialect>()
            || dialect.is::<MySqlDialect>()
            || dialect.is::<AnsiDialect>();
        // SQL Server also concatenates strings with `+`, where the order matters.
        let plus_is_numeric = true_is_reserved || dialect.is::<SQLiteDialect>();
        Self {
            column,
            qualifier,
            true_is_reserved,
            plus_is_numeric,
            // SQLite compares two columns under the left one's collation.
            collation_is_symmetric: true_is_reserved,
        }
    }
}

fn name(ident: &Ident, fold: Fold) -> String {
    let quoted = ident.quote_style.is_some();
    match fold {
        Fold::Lower if !quoted => ident.value.to_ascii_lowercase(),
        Fold::Upper if !quoted => ident.value.to_ascii_uppercase(),
        Fold::Insensitive => ident.value.to_ascii_lowercase(),
        _ => ident.value.clone(),
    }
}

fn flatten(expr: &Expr, operator: &BinaryOperator, folding: Folding, out: &mut Vec<String>) {
    match expr {
        Expr::Nested(inner) => flatten(inner, operator, folding, out),
        Expr::BinaryOp { left, op, right } if op == operator => {
            flatten(left, operator, folding, out);
            flatten(right, operator, folding, out);
        }
        _ => out.push(meaning(expr, folding)),
    }
}

fn unordered(left: &Expr, right: &Expr, folding: Folding) -> (String, String) {
    let (left, right) = (meaning(left, folding), meaning(right, folding));
    if left > right {
        (right, left)
    } else {
        (left, right)
    }
}

/// Returns the meaning text of `expr`.
pub fn meaning(expr: &Expr, folding: Folding) -> String {
    let m = |expr: &Expr| meaning(expr, folding);
    match expr {
        Expr::Nested(inner) => m(inner),
        Expr::Identifier(ident) => name(ident, folding.column),
        Expr::CompoundIdentifier(parts) => {
            let last = parts.len().saturating_sub(1);
            parts
                .iter()
                .enumerate()
                .map(|(index, part)| {
                    let fold = if index == last {
                        folding.column
                    } else {
                        folding.qualifier
                    };
                    name(part, fold)
                })
                .collect::<Vec<_>>()
                .join(".")
        }
        Expr::BinaryOp { op, .. } if matches!(op, BinaryOperator::And | BinaryOperator::Or) => {
            let mut children = Vec::new();
            flatten(expr, op, folding, &mut children);
            children.sort();
            format!("({op} {})", children.join(" "))
        }
        Expr::BinaryOp { left, op, right }
            if matches!(
                op,
                BinaryOperator::Eq | BinaryOperator::NotEq | BinaryOperator::Spaceship
            ) =>
        {
            let (left, right) = compared(left, right, folding);
            format!("({op} {left} {right})")
        }
        // `a > b` and `b < a` are one comparison wherever the operands' collation does not depend
        // on their order.
        Expr::BinaryOp {
            left,
            op: op @ (BinaryOperator::Gt | BinaryOperator::GtEq),
            right,
        } if swappable(left, right, folding) => {
            let mirrored = if matches!(op, BinaryOperator::Gt) {
                "<"
            } else {
                "<="
            };
            format!("({mirrored} {} {})", m(right), m(left))
        }
        Expr::BinaryOp { left, op, right }
            if folding.plus_is_numeric
                && matches!(op, BinaryOperator::Plus | BinaryOperator::Multiply) =>
        {
            let (left, right) = unordered(left, right, folding);
            format!("({op} {left} {right})")
        }
        Expr::IsDistinctFrom(left, right)
        | Expr::BinaryOp {
            left,
            op: BinaryOperator::IsNot,
            right,
        } => {
            let (left, right) = compared(left, right, folding);
            format!("(distinct {left} {right})")
        }
        Expr::IsNotDistinctFrom(left, right)
        | Expr::BinaryOp {
            left,
            op: BinaryOperator::Is,
            right,
        } => {
            let (left, right) = compared(left, right, folding);
            format!("(not-distinct {left} {right})")
        }
        Expr::BinaryOp { left, op, right } => format!("({op} {} {})", m(left), m(right)),
        Expr::UnaryOp { op, expr } => match (op, strip_nested(expr)) {
            // `NOT (EXISTS q)` and `NOT EXISTS q` are one test.
            (
                UnaryOperator::Not,
                Expr::Exists {
                    subquery,
                    negated: false,
                },
            ) => format!("(exists true {})", query_meaning(subquery, folding)),
            _ => format!("({op} {})", m(expr)),
        },
        Expr::IsNull(expr) => format!("(is-null {})", m(expr)),
        Expr::IsNotNull(expr) => format!("(is-not-null {})", m(expr)),
        Expr::IsTrue(expr) => format!("(is-true {})", m(expr)),
        Expr::IsNotTrue(expr) => format!("(is-not-true {})", m(expr)),
        Expr::IsFalse(expr) => format!("(is-false {})", m(expr)),
        Expr::IsNotFalse(expr) => format!("(is-not-false {})", m(expr)),
        Expr::IsUnknown(expr) => format!("(is-unknown {})", m(expr)),
        Expr::IsNotUnknown(expr) => format!("(is-not-unknown {})", m(expr)),
        Expr::In {
            expr,
            target: InTarget::List(list),
            negated,
            global,
        } => {
            let mut items: Vec<String> = list.iter().map(m).collect();
            items.sort();
            format!("(in {global} {negated} {} [{}])", m(expr), items.join(" "))
        }
        Expr::In {
            expr,
            target: InTarget::Subquery(subquery),
            negated,
            global,
        } => format!(
            "(in-subquery {global} {negated} {} {})",
            m(expr),
            query_meaning(subquery, folding)
        ),
        Expr::In {
            expr,
            target: InTarget::Table { name, args },
            negated,
            global,
        } => {
            let args = args.as_ref().map(|args| {
                args.iter()
                    .map(|arg| match arg {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)) => m(arg),
                        other => other.to_string(),
                    })
                    .collect::<Vec<_>>()
            });
            format!(
                "(in-table {global} {negated} {} {} {args:?})",
                m(expr),
                object_name(name, folding)
            )
        }
        Expr::Exists { subquery, negated } => {
            format!("(exists {negated} {})", query_meaning(subquery, folding))
        }
        Expr::Subquery(query) => format!("(subquery {})", query_meaning(query, folding)),
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => format!("(between {negated} {} {} {})", m(expr), m(low), m(high)),
        Expr::Like {
            negated,
            any,
            expr,
            pattern,
            escape_char,
        } => format!(
            "(like {negated} {any} {} {} {:?})",
            m(expr),
            m(pattern),
            escape_char.as_deref().map(m)
        ),
        Expr::ILike {
            negated,
            any,
            expr,
            pattern,
            escape_char,
        } => format!(
            "(ilike {negated} {any} {} {} {:?})",
            m(expr),
            m(pattern),
            escape_char.as_deref().map(m)
        ),
        Expr::SimilarTo {
            negated,
            expr,
            pattern,
            escape_char,
        } => {
            let escape = escape_char.as_deref().map(m);
            format!("(similar {negated} {} {} {escape:?})", m(expr), m(pattern))
        }
        // `REGEXP` and `RLIKE` are one operator.
        Expr::RLike {
            negated,
            expr,
            pattern,
            ..
        } => format!("(rlike {negated} {} {})", m(expr), m(pattern)),
        // `SOME` and `ANY` are one quantifier.
        Expr::AnyOp {
            left,
            compare_op,
            right,
            ..
        } => format!("(any {compare_op} {} {})", m(left), m(right)),
        Expr::AllOp {
            left,
            compare_op,
            right,
        } => format!("(all {compare_op} {} {})", m(left), m(right)),
        Expr::IsJson {
            expr,
            kind,
            unique_keys,
            negated,
        } => format!("(is-json {negated} {} {kind:?} {unique_keys:?})", m(expr)),
        Expr::IsNormalized {
            expr,
            form,
            negated,
        } => format!("(is-normalized {negated} {} {form:?})", m(expr)),
        Expr::MemberOf(member) => {
            format!("(member-of {} {})", m(&member.value), m(&member.array))
        }
        Expr::AtTimeZone {
            timestamp,
            time_zone,
        } => format!("(at-time-zone {} {})", m(timestamp), m(time_zone)),
        Expr::Collate { expr, collation } => format!("(collate {} {collation})", m(expr)),
        Expr::MatchAgainst {
            columns,
            match_value,
            opt_search_modifier,
        } => {
            let columns: Vec<String> = columns
                .iter()
                .map(|column| object_name(column, folding))
                .collect();
            format!(
                "(match [{}] {match_value} {opt_search_modifier:?})",
                columns.join(" ")
            )
        }
        Expr::Function(function) => {
            // A quoted function name is another lookup in MySQL, so the oracle never merges it
            // with the bare spelling.
            let quoted = function.name.0.iter().any(|part| {
                part.as_ident()
                    .is_some_and(|ident| ident.quote_style.is_some())
            });
            let name = format!("{quoted}:{}", object_name(&function.name, folding));
            let plain = function.filter.is_none()
                && function.over.is_none()
                && function.within_group.is_empty()
                && function.null_treatment.is_none()
                && matches!(function.parameters, FunctionArguments::None);
            match &function.args {
                FunctionArguments::None if plain => format!("(call {name})"),
                FunctionArguments::List(list)
                    if plain && list.duplicate_treatment.is_none() && list.clauses.is_empty() =>
                {
                    let args: Vec<String> = list
                        .args
                        .iter()
                        .map(|arg| match arg {
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)) => m(arg),
                            other => other.to_string(),
                        })
                        .collect();
                    format!("(call {name} [{}])", args.join(" "))
                }
                _ => function.to_string(),
            }
        }
        // `x::T` and `CAST(x AS T)` are one cast.
        Expr::Cast {
            kind,
            expr,
            data_type,
            format,
        } => {
            let kind = match kind {
                CastKind::DoubleColon => &CastKind::Cast,
                kind => kind,
            };
            let format = format.as_ref().map(ToString::to_string);
            format!("(cast {kind:?} {} {data_type} {format:?})", m(expr))
        }
        Expr::Convert {
            is_try,
            expr,
            data_type,
            charset,
            target_before_value,
            styles,
        } => {
            let data_type = data_type.as_ref().map(ToString::to_string);
            let charset = charset.as_ref().map(ToString::to_string);
            let styles: Vec<String> = styles.iter().map(m).collect();
            format!(
                "(convert {is_try} {} {data_type:?} {charset:?} {target_before_value} [{}])",
                m(expr),
                styles.join(" ")
            )
        }
        Expr::Extract {
            field,
            syntax,
            expr,
        } => format!("(extract {field} {syntax:?} {})", m(expr)),
        Expr::Ceil { expr, field } => format!("(ceil {} {})", m(expr), rounding(field)),
        Expr::Floor { expr, field } => format!("(floor {} {})", m(expr), rounding(field)),
        Expr::Position { expr, r#in } => format!("(position {} {})", m(expr), m(r#in)),
        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            special,
            shorthand,
        } => format!(
            "(substring {special} {shorthand} {} {:?} {:?})",
            m(expr),
            substring_from.as_deref().map(m),
            substring_for.as_deref().map(m)
        ),
        Expr::Trim {
            expr,
            trim_where,
            trim_what,
            trim_characters,
        } => {
            let characters = trim_characters
                .as_ref()
                .map(|characters| characters.iter().map(m).collect::<Vec<_>>());
            format!(
                "(trim {trim_where:?} {} {:?} {characters:?})",
                m(expr),
                trim_what.as_deref().map(m)
            )
        }
        Expr::Overlay {
            expr,
            overlay_what,
            overlay_from,
            overlay_for,
        } => format!(
            "(overlay {} {} {} {:?})",
            m(expr),
            m(overlay_what),
            m(overlay_from),
            overlay_for.as_deref().map(m)
        ),
        // A missing `ELSE` and `ELSE NULL` are one result.
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let branches: Vec<String> = conditions
                .iter()
                .map(|when| format!("{} {}", m(&when.condition), m(&when.result)))
                .collect();
            let otherwise = else_result
                .as_deref()
                .filter(|result| !is_null(result))
                .map(m);
            format!(
                "(case {:?} [{}] {otherwise:?})",
                operand.as_deref().map(m),
                branches.join(" ")
            )
        }
        Expr::Tuple(items) => {
            let items: Vec<String> = items.iter().map(m).collect();
            format!("(tuple [{}])", items.join(" "))
        }
        Expr::Array(array) => {
            let items: Vec<String> = array.elem.iter().map(m).collect();
            format!("(array {} [{}])", array.named, items.join(" "))
        }
        Expr::Interval(interval) => format!(
            "(interval {} {:?} {:?} {:?} {:?})",
            m(&interval.value),
            interval.leading_field.as_ref().map(ToString::to_string),
            interval.leading_precision,
            interval.last_field.as_ref().map(ToString::to_string),
            interval.fractional_seconds_precision
        ),
        Expr::Prefixed { prefix, value } => format!("(prefixed {prefix} {})", m(value)),
        // A parenthesized name is a value, while a bare name before a dot may be a table.
        Expr::CompoundFieldAccess { root, access_chain } => {
            let root = match root.as_ref() {
                Expr::Nested(inner) if is_name(inner) => format!("(value {})", m(inner)),
                root => access_name(root, folding),
            };
            let chain: Vec<String> = access_chain
                .iter()
                .map(|access| match access {
                    AccessExpr::Dot(field) => format!(".{}", access_name(field, folding)),
                    AccessExpr::Subscript(Subscript::Index { index }) => format!("[{}]", m(index)),
                    AccessExpr::Subscript(Subscript::Slice {
                        lower_bound,
                        upper_bound,
                        stride,
                    }) => format!(
                        "[{:?}:{:?}:{:?}]",
                        lower_bound.as_ref().map(m),
                        upper_bound.as_ref().map(m),
                        stride.as_ref().map(m)
                    ),
                })
                .collect();
            format!("(access {root} {})", chain.concat())
        }
        other => format!("{other}"),
    }
}

fn object_name(name: &ObjectName, folding: Folding) -> String {
    let last = name.0.len().saturating_sub(1);
    name.0
        .iter()
        .enumerate()
        .map(|(index, part)| {
            let fold = if index == last {
                folding.column
            } else {
                folding.qualifier
            };
            part.as_ident()
                .map_or_else(|| part.to_string(), |ident| self::name(ident, fold))
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn access_name(expr: &Expr, folding: Folding) -> String {
    match expr {
        Expr::Identifier(ident) => name(ident, folding.qualifier),
        expr => meaning(expr, folding),
    }
}

fn rounding(field: &CeilFloorKind) -> String {
    match field {
        CeilFloorKind::DateTimeField(field) => field.to_string(),
        CeilFloorKind::Scale(scale) => scale.to_string(),
    }
}

fn is_null(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => is_null(inner),
        Expr::Value(value) => matches!(value.value, Value::Null),
        _ => false,
    }
}

fn is_name(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => is_name(inner),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => true,
        _ => false,
    }
}

/// Meaning of a subquery in the shape the canonicalizer serves, and its printed text otherwise.
fn query_meaning(query: &Query, folding: Folding) -> String {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return query.to_string();
    };
    let simple = query.with.is_none()
        && query.order_by.is_none()
        && query.limit_clause.is_none()
        && select.distinct.is_none()
        && select.having.is_none()
        && matches!(&select.group_by, GroupByExpr::Expressions(exprs, _) if exprs.is_empty())
        && select.from.len() == 1
        && select.from[0].joins.is_empty();
    if !simple {
        return query.to_string();
    }
    let TableFactor::Table {
        name,
        alias,
        inheritance,
        ..
    } = &select.from[0].relation
    else {
        return query.to_string();
    };
    // `ONLY` leaves out descendant tables, which a bare name and `name *` both read.
    let only = matches!(inheritance, Some(TableInheritance::Only { .. }));
    // A table alias only names the table, so its meaning is its folded name.
    let alias = match alias {
        None => None,
        Some(alias) if alias.columns.is_empty() && alias.at.is_none() => {
            Some(self::name(&alias.name, folding.qualifier))
        }
        Some(_) => return query.to_string(),
    };
    let items: Vec<String> = select
        .projection
        .iter()
        .map(|item| match item {
            SelectItem::UnnamedExpr(expr) => meaning(expr, folding),
            other => other.to_string(),
        })
        .collect();
    let table: Vec<String> = name
        .0
        .iter()
        .map(|part| {
            part.as_ident().map_or_else(
                || part.to_string(),
                |ident| self::name(ident, folding.qualifier),
            )
        })
        .collect();
    // `WHERE TRUE` keeps every row, as a missing `WHERE` does.
    let filter = select
        .selection
        .as_ref()
        .filter(|filter| !(folding.true_is_reserved && is_true(filter)))
        .map(|filter| meaning(filter, folding));
    format!(
        "(select [{}] {only} {} {alias:?} {filter:?})",
        items.join(" "),
        table.join(".")
    )
}

fn strip_nested(expr: &Expr) -> &Expr {
    match expr {
        Expr::Nested(inner) => strip_nested(inner),
        expr => expr,
    }
}

fn is_true(expr: &Expr) -> bool {
    matches!(strip_nested(expr), Expr::Value(value) if matches!(value.value, Value::Boolean(true)))
}

/// Reports whether swapping `left` and `right` keeps the comparison, which fails where the left
/// operand's collation wins and neither side is a literal.
fn swappable(left: &Expr, right: &Expr, folding: Folding) -> bool {
    folding.collation_is_symmetric
        || literal(left, folding)
        || literal(right, folding)
        || meaning(left, folding) == meaning(right, folding)
}

/// A boolean is a literal only where `TRUE` is reserved, as SQLite may read it as a column.
fn literal(expr: &Expr, folding: Folding) -> bool {
    match strip_nested(expr) {
        Expr::UnaryOp {
            op: UnaryOperator::Minus | UnaryOperator::Plus,
            expr,
        } => literal(expr, folding),
        Expr::Value(value) => folding.true_is_reserved || !matches!(value.value, Value::Boolean(_)),
        _ => false,
    }
}

/// Meaning texts of the two operands of a symmetric comparison, sorted where swapping them keeps
/// the comparison.
fn compared(left: &Expr, right: &Expr, folding: Folding) -> (String, String) {
    if swappable(left, right, folding) {
        unordered(left, right, folding)
    } else {
        (meaning(left, folding), meaning(right, folding))
    }
}
