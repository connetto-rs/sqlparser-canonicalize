use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::hash::{Hash, Hasher};

use seahash::SeaHasher;
use sqlparser::ast::{
    AccessExpr, BinaryOperator, CastKind, CeilFloorKind, DateTimeField, Distinct, Expr,
    ExtractSyntax, Function, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr, Ident,
    InTarget, Interval, LimitClause, ObjectName, Query, Select, SelectItem, SelectModifiers,
    SetExpr, Statement, Subscript, TableAlias, TableFactor, TableInheritance, UnaryOperator, Value,
};
use sqlparser::dialect::{AnsiDialect, Dialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::keywords::ALL_KEYWORDS;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Token;

use crate::CanonicalizeError;

const MAX_EXPR_DEPTH: usize = 128;
const MAX_SQL_LEN: usize = 8192;

impl<'a> Canonicalizer<'a> {
    /// Parses one `SELECT` and returns canonical text for its `WHERE` clause.
    ///
    /// The canonical text is verified to survive being read back as itself, so a predicate
    /// whose canonical form would read as something else is rejected instead of hashed.
    pub fn normalize_sql(&self, sql: &str) -> Result<String, CanonicalizeError> {
        let canonical = normalize_sql_inner(sql, self)?;
        confirm_reads_back_as_itself(canonical, self)
    }

    /// Canonicalizes a parsed `WHERE` clause in O(n log n) time for boolean chains and O(n)
    /// otherwise.
    ///
    /// `where_expr` MUST have been parsed with this canonicalizer's dialect, which decides
    /// how names fold and which reads the canonical text back to check it.
    pub fn normalize_where_clause(
        &self,
        where_expr: Option<&Expr>,
    ) -> Result<String, CanonicalizeError> {
        let canonical = normalize_where_clause_inner(where_expr, self)?;
        confirm_reads_back_as_itself(canonical, self)
    }
}

fn normalize_sql_inner(
    sql: &str,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    if sql.len() > MAX_SQL_LEN {
        return Err(CanonicalizeError::InputTooLong { limit: MAX_SQL_LEN });
    }
    check_sql_sanity(sql)?;

    let statements = Parser::parse_sql(context.dialect, sql)
        .map_err(|error| CanonicalizeError::Parse(error.to_string()))?;
    let [statement] = statements.as_slice() else {
        return Err(CanonicalizeError::Unsupported(
            "Expected exactly one SELECT statement".to_string(),
        ));
    };
    let where_expr = extract_where(statement)?;
    normalize_where_clause_inner(where_expr, context)
}

fn normalize_where_clause_inner(
    where_expr: Option<&Expr>,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    match where_expr {
        Some(expr) => normalize_expr_inner(expr, 0, false, context),
        // A missing filter keeps every row. Where `TRUE` is reserved it is spelled as
        // `WHERE TRUE`. Elsewhere a bare `TRUE` may name a column, as in SQLite, so it is spelled
        // as `WHERE 1 = 1`, which keeps every row in every dialect.
        None => {
            let every_row = if context.true_is_reserved {
                Expr::Value(Value::Boolean(true).with_empty_span())
            } else {
                let one = || {
                    Box::new(Expr::Value(
                        Value::Number("1".into(), false).with_empty_span(),
                    ))
                };
                Expr::BinaryOp {
                    left: one(),
                    op: BinaryOperator::Eq,
                    right: one(),
                }
            };
            normalize_expr_inner(&every_row, 0, false, context)
        }
    }
}

/// Returns the canonical text only if reading it back as an expression reproduces it byte
/// for byte.
///
/// Canonical text is a hash key, so text that reads back as a different predicate would
/// give two distinct predicates one hash. An expression re-read checks the whole rendering
/// because a statement around it adds only scaffolding no caller sent.
fn confirm_reads_back_as_itself(
    canonical: String,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    if read_back(&canonical, context).is_some_and(|again| again == canonical) {
        Ok(canonical)
    } else {
        Err(CanonicalizeError::NotRoundTrippable(canonical))
    }
}

/// Reads `canonical` back as one complete expression and returns its normalised text.
fn read_back(canonical: &str, context: &Canonicalizer<'_>) -> Option<String> {
    let mut parser = Parser::new(context.dialect).try_with_sql(canonical).ok()?;
    let expr = parser.parse_expr().ok()?;
    // parse_expr takes a leading expression and stops, so require nothing to trail it.
    if !matches!(parser.peek_token_ref().token, Token::EOF) {
        return None;
    }
    normalize_expr_inner(&expr, 0, false, context).ok()
}

/// Returns the stable 128-bit SeaHash value for canonical text.
#[must_use]
pub fn hash_canonical(normalized: &str) -> u128 {
    let mut first = SeaHasher::new();
    normalized.hash(&mut first);
    let first = first.finish();

    let mut second = SeaHasher::with_seeds(
        first,
        first.wrapping_add(1),
        first.wrapping_add(2),
        first.wrapping_add(3),
    );
    normalized.hash(&mut second);
    (u128::from(first) << 64) | u128::from(second.finish())
}

fn check_sql_sanity(sql: &str) -> Result<(), CanonicalizeError> {
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut consecutive_ops = 0usize;

    for byte in sql.bytes() {
        match byte {
            b'(' => {
                paren_depth += 1;
                consecutive_ops += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                consecutive_ops = 0;
            }
            b'[' => {
                bracket_depth += 1;
                consecutive_ops = 0;
            }
            b']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                consecutive_ops = 0;
            }
            b'+' | b'-' | b'*' | b'/' | b'=' | b'<' | b'>' | b'!' | b'~' => {
                consecutive_ops += 1;
            }
            b' ' | b'\t' | b'\n' | b'\r' => {}
            0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F => {
                return Err(CanonicalizeError::Unsupported(
                    "Control character in SQL".to_string(),
                ));
            }
            _ => consecutive_ops = 0,
        }

        if paren_depth > MAX_EXPR_DEPTH
            || bracket_depth > MAX_EXPR_DEPTH
            || consecutive_ops > MAX_EXPR_DEPTH
        {
            return Err(CanonicalizeError::TooDeep {
                limit: MAX_EXPR_DEPTH,
            });
        }
    }

    if paren_depth != 0 {
        return Err(CanonicalizeError::Unsupported(
            "Unbalanced parentheses".to_string(),
        ));
    }
    if bracket_depth != 0 {
        return Err(CanonicalizeError::Unsupported(
            "Unbalanced square brackets".to_string(),
        ));
    }
    Ok(())
}

fn extract_where(statement: &Statement) -> Result<Option<&Expr>, CanonicalizeError> {
    let Statement::Query(query) = statement else {
        return Err(CanonicalizeError::Unsupported(
            "Only SELECT statements are supported".to_string(),
        ));
    };
    Ok(single_table_select(query)?.selection.as_ref())
}

fn single_table_select(query: &Query) -> Result<&Select, CanonicalizeError> {
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(CanonicalizeError::Unsupported(
            "Set operations are not supported".to_string(),
        ));
    };
    if select.from.len() != 1 {
        return Err(CanonicalizeError::Unsupported(
            "Exactly one table is required".to_string(),
        ));
    }
    if !select.from[0].joins.is_empty() {
        return Err(CanonicalizeError::Unsupported(
            "JOINs not supported".to_string(),
        ));
    }
    if !matches!(select.from[0].relation, TableFactor::Table { .. }) {
        return Err(CanonicalizeError::Unsupported(
            "Subqueries and derived tables not supported".to_string(),
        ));
    }
    check_served_clauses(query, select)?;
    Ok(select)
}

fn check_served_clauses(query: &Query, select: &Select) -> Result<(), CanonicalizeError> {
    let Query {
        with,
        body: _,
        order_by: _,
        limit_clause,
        fetch,
        locks,
        for_clause,
        settings,
        format_clause,
        pipe_operators,
    } = query;
    let Select {
        select_token: _,
        optimizer_hints,
        distinct,
        select_modifiers,
        top,
        top_before_distinct: _,
        projection: _,
        exclude,
        into,
        from: _,
        lateral_views,
        prewhere,
        selection: _,
        connect_by,
        group_by: _,
        cluster_by,
        distribute_by,
        sort_by,
        having: _,
        named_window,
        qualify,
        window_before_qualify: _,
        value_table_mode,
        flavor: _,
    } = select;

    let deduplicating = !matches!(distinct, None | Some(Distinct::All));
    for (present, clause) in [
        (with.is_some(), "WITH"),
        (deduplicating, "DISTINCT"),
        (
            limit_clause.is_some(),
            limit_clause_name(limit_clause.as_ref()),
        ),
        (fetch.is_some(), "FETCH"),
        (!locks.is_empty(), "FOR UPDATE or FOR SHARE"),
        (for_clause.is_some(), "FOR XML or FOR JSON"),
        (settings.is_some(), "SETTINGS"),
        (format_clause.is_some(), "FORMAT"),
        (!pipe_operators.is_empty(), "pipe operators"),
        (!optimizer_hints.is_empty(), "optimizer hints"),
        (
            select_modifiers
                .as_ref()
                .is_some_and(SelectModifiers::is_any_set),
            "SELECT modifiers",
        ),
        (top.is_some(), "TOP"),
        (exclude.is_some(), "EXCLUDE"),
        (into.is_some(), "INTO"),
        (!lateral_views.is_empty(), "LATERAL VIEW"),
        (prewhere.is_some(), "PREWHERE"),
        (!connect_by.is_empty(), "CONNECT BY"),
        (!cluster_by.is_empty(), "CLUSTER BY"),
        (!distribute_by.is_empty(), "DISTRIBUTE BY"),
        (!sort_by.is_empty(), "SORT BY"),
        (!named_window.is_empty(), "WINDOW"),
        (qualify.is_some(), "QUALIFY"),
        (
            value_table_mode.is_some(),
            "SELECT AS VALUE or SELECT AS STRUCT",
        ),
    ] {
        if present {
            return Err(CanonicalizeError::Unsupported(format!(
                "{clause} is not supported"
            )));
        }
    }
    Ok(())
}

const fn limit_clause_name(limit: Option<&LimitClause>) -> &'static str {
    match limit {
        Some(LimitClause::LimitOffset {
            limit: None,
            offset,
            limit_by,
        }) => {
            if offset.is_some() {
                "OFFSET"
            } else if limit_by.is_empty() {
                "LIMIT"
            } else {
                "LIMIT BY"
            }
        }
        None | Some(LimitClause::LimitOffset { .. } | LimitClause::OffsetCommaLimit { .. }) => {
            "LIMIT"
        }
    }
}

fn normalize_expr_inner(
    expr: &Expr,
    depth: usize,
    tight_parent: bool,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    if depth > MAX_EXPR_DEPTH {
        return Err(CanonicalizeError::TooDeep {
            limit: MAX_EXPR_DEPTH,
        });
    }

    let text = match expr {
        Expr::BinaryOp { left, op, right } => {
            if matches!(op, BinaryOperator::And | BinaryOperator::Or) {
                let mut children = collect_flat_children(left, op);
                children.extend(collect_flat_children(right, op));
                let mut child_text: Vec<String> = children
                    .iter()
                    .map(|child| normalize_expr_inner(child, depth + 1, false, context))
                    .collect::<Result<_, _>>()?;
                child_text.sort();
                // One level of parentheses around the whole chain keeps reading it back
                // within the parser's recursion limit however long the chain is.
                let operator = operator_text(op)?;
                let length = child_text.iter().map(String::len).sum::<usize>()
                    + (child_text.len() - 1) * (operator.len() + 2)
                    + 2;
                let mut text = String::with_capacity(length);
                text.push('(');
                for (index, child) in child_text.iter().enumerate() {
                    if index > 0 {
                        text.push(' ');
                        text.push_str(operator);
                        text.push(' ');
                    }
                    text.push_str(child);
                }
                text.push(')');
                text
            } else {
                let operator = operator_text(op)?;
                let order = operand_order(op, context);
                infix_text(left, operator, right, order, depth, context)?
            }
        }
        Expr::IsDistinctFrom(left, right) => {
            let order = OperandOrder::Compared;
            infix_text(left, "IS DISTINCT FROM", right, order, depth, context)?
        }
        Expr::IsNotDistinctFrom(left, right) => {
            let order = OperandOrder::Compared;
            infix_text(left, "IS NOT DISTINCT FROM", right, order, depth, context)?
        }
        Expr::UnaryOp { op, expr } => format!(
            "{} {}",
            unary_operator_text(op)?,
            normalize_expr_inner(expr, depth + 1, true, context)?
        ),
        Expr::IsNull(operand) => postfix_text(operand, "IS NULL", depth, context)?,
        Expr::IsNotNull(operand) => postfix_text(operand, "IS NOT NULL", depth, context)?,
        Expr::IsTrue(operand) => postfix_text(operand, "IS TRUE", depth, context)?,
        Expr::IsNotTrue(operand) => postfix_text(operand, "IS NOT TRUE", depth, context)?,
        Expr::IsFalse(operand) => postfix_text(operand, "IS FALSE", depth, context)?,
        Expr::IsNotFalse(operand) => postfix_text(operand, "IS NOT FALSE", depth, context)?,
        Expr::IsUnknown(operand) => postfix_text(operand, "IS UNKNOWN", depth, context)?,
        Expr::IsNotUnknown(operand) => postfix_text(operand, "IS NOT UNKNOWN", depth, context)?,
        Expr::IsJson {
            expr,
            kind,
            unique_keys,
            negated,
        } => {
            let not = if *negated { "NOT " } else { "" };
            let mut suffix = format!("IS {not}JSON");
            if let Some(kind) = kind {
                suffix.push_str(&format!(" {kind}"));
            }
            if let Some(unique_keys) = unique_keys {
                suffix.push_str(&format!(" {unique_keys}"));
            }
            postfix_text(expr, &suffix, depth, context)?
        }
        Expr::IsNormalized {
            expr,
            form,
            negated,
        } => {
            let not = if *negated { "NOT " } else { "" };
            let suffix = form.as_ref().map_or_else(
                || format!("IS {not}NORMALIZED"),
                |form| format!("IS {not}{form} NORMALIZED"),
            );
            postfix_text(expr, &suffix, depth, context)?
        }
        Expr::In {
            expr,
            target,
            negated,
            global,
        } => {
            let modifiers = membership_modifiers(*global, *negated);
            match target {
                InTarget::List(list) => {
                    let mut items: Vec<String> = list
                        .iter()
                        .map(|item| normalize_expr_inner(item, depth + 1, true, context))
                        .collect::<Result<_, _>>()?;
                    items.sort();
                    open_list(&mut items, context);
                    let mut text = format!(
                        "{} {modifiers}IN (",
                        normalize_expr_inner(expr, depth + 1, true, context)?
                    );
                    for (index, item) in items.iter().enumerate() {
                        if index > 0 {
                            text.push_str(", ");
                        }
                        text.push_str(item);
                    }
                    text.push(')');
                    text
                }
                InTarget::Subquery(subquery) => format!(
                    "{} {modifiers}IN ({})",
                    normalize_expr_inner(expr, depth + 1, true, context)?,
                    subquery_text(subquery, depth, context)?
                ),
                InTarget::Table { name, args } => {
                    let mut text = format!(
                        "{} {modifiers}IN {}",
                        normalize_expr_inner(expr, depth + 1, true, context)?,
                        object_name_text(name, NamePlace::Operand, context)?
                    );
                    if let Some(args) = args {
                        text.push('(');
                        append_function_args(&mut text, args, depth, context)?;
                        text.push(')');
                    }
                    text
                }
                InTarget::Unnest(_) => return Err(refused("IN UNNEST")),
            }
        }
        Expr::Exists { subquery, negated } => {
            let not = if *negated { "NOT " } else { "" };
            format!("{not}EXISTS ({})", subquery_text(subquery, depth, context)?)
        }
        Expr::Subquery(query) => format!("({})", subquery_text(query, depth, context)?),
        Expr::Between {
            expr,
            low,
            high,
            negated,
        } => {
            let not = if *negated { "NOT " } else { "" };
            format!(
                "{} {not}BETWEEN {} AND {}",
                normalize_expr_inner(expr, depth + 1, true, context)?,
                normalize_expr_inner(low, depth + 1, true, context)?,
                normalize_expr_inner(high, depth + 1, true, context)?
            )
        }
        Expr::Like {
            negated,
            any,
            expr,
            pattern,
            escape_char,
        } => {
            let operator = match_operator(*negated, "LIKE");
            let escape = escape_char.as_deref();
            pattern_match_text(expr, &operator, *any, pattern, escape, depth, context)?
        }
        Expr::ILike {
            negated,
            any,
            expr,
            pattern,
            escape_char,
        } => {
            let operator = match_operator(*negated, "ILIKE");
            let escape = escape_char.as_deref();
            pattern_match_text(expr, &operator, *any, pattern, escape, depth, context)?
        }
        Expr::SimilarTo {
            negated,
            expr,
            pattern,
            escape_char,
        } => {
            let operator = match_operator(*negated, "SIMILAR TO");
            let escape = escape_char.as_deref();
            pattern_match_text(expr, &operator, false, pattern, escape, depth, context)?
        }
        // `REGEXP` and `RLIKE` are one operator. `RLIKE` is the spelling every dialect reads
        // back as it, because SQLite reads `REGEXP` as an operator of its own.
        Expr::RLike {
            negated,
            expr,
            pattern,
            regexp: _,
        } => {
            let operator = match_operator(*negated, "RLIKE");
            pattern_match_text(expr, &operator, false, pattern, None, depth, context)?
        }
        // `SOME` and `ANY` are one quantifier.
        Expr::AnyOp {
            left,
            compare_op,
            right,
            is_some: _,
        } => quantified_text(left, compare_op, "ANY", right, depth, context)?,
        Expr::AllOp {
            left,
            compare_op,
            right,
        } => quantified_text(left, compare_op, "ALL", right, depth, context)?,
        Expr::MemberOf(member) => format!(
            "{} MEMBER OF({})",
            normalize_expr_inner(&member.value, depth + 1, true, context)?,
            normalize_expr_inner(&member.array, depth + 1, false, context)?
        ),
        Expr::AtTimeZone {
            timestamp,
            time_zone,
        } => format!(
            "{} AT TIME ZONE {}",
            normalize_expr_inner(timestamp, depth + 1, true, context)?,
            normalize_expr_inner(time_zone, depth + 1, true, context)?
        ),
        // A collation name is kept as written, because whether it is case sensitive differs
        // by dialect and by server.
        Expr::Collate { expr, collation } => format!(
            "{} COLLATE {collation}",
            normalize_expr_inner(expr, depth + 1, true, context)?
        ),
        Expr::MatchAgainst {
            columns,
            match_value,
            opt_search_modifier,
        } => {
            let columns = columns
                .iter()
                .map(|column| object_name_text(column, NamePlace::Operand, context))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            reject_lossy_quoting(&match_value.value)?;
            let modifier = opt_search_modifier
                .as_ref()
                .map_or_else(String::new, |modifier| format!(" {modifier}"));
            format!(
                "MATCH ({columns}) AGAINST ({}{modifier})",
                match_value.value
            )
        }
        Expr::Nested(inner) => normalize_expr_inner(inner, depth + 1, tight_parent, context)?,
        Expr::Identifier(identifier) => {
            identifier_text(identifier, context.folding, NamePlace::Operand, context)?
        }
        Expr::CompoundIdentifier(parts) => {
            qualified_name_text(parts.iter(), NamePlace::Operand, context)?
        }
        Expr::Value(value) => {
            reject_lossy_quoting(&value.value)?;
            format!("{}", value.value)
        }
        // sqlparser reads `:` as a JSON path even in dialects without one, and can build a
        // tree its printer spells with another grouping.
        Expr::JsonAccess { .. } => {
            return Err(CanonicalizeError::Unsupported(
                "JSON path access is not supported".to_string(),
            ));
        }
        Expr::Function(function) => function_text(function, depth, context)?,
        // `x::T` and `CAST(x AS T)` are one cast, and every dialect reads the `CAST` spelling.
        Expr::Cast {
            kind,
            expr,
            data_type,
            format,
        } => {
            if format.is_some() {
                return Err(CanonicalizeError::Unsupported(
                    "CAST with FORMAT is not supported".to_string(),
                ));
            }
            let keyword = match kind {
                CastKind::Cast | CastKind::DoubleColon => "CAST",
                CastKind::TryCast => "TRY_CAST",
                CastKind::SafeCast => "SAFE_CAST",
            };
            let expr = normalize_expr_inner(expr, depth + 1, false, context)?;
            format!("{keyword}({expr} AS {data_type})")
        }
        Expr::Convert {
            is_try,
            expr,
            data_type,
            charset,
            target_before_value,
            styles,
        } => {
            // The SQL Server form puts the type first and takes styles.
            if *target_before_value || !styles.is_empty() {
                return Err(CanonicalizeError::Unsupported(
                    "CONVERT with the type first is not supported".to_string(),
                ));
            }
            let target = match (data_type, charset) {
                (Some(data_type), Some(charset)) => {
                    format!(", {data_type} CHARACTER SET {charset}")
                }
                (Some(data_type), None) => format!(", {data_type}"),
                (None, Some(charset)) => format!(" USING {charset}"),
                (None, None) => {
                    return Err(CanonicalizeError::Unsupported(
                        "CONVERT without a target is not supported".to_string(),
                    ));
                }
            };
            let prefix = if *is_try { "TRY_" } else { "" };
            let expr = normalize_expr_inner(expr, depth + 1, false, context)?;
            format!("{prefix}CONVERT({expr}{target})")
        }
        Expr::Extract {
            field,
            syntax,
            expr,
        } => {
            let expr = normalize_expr_inner(expr, depth + 1, false, context)?;
            match syntax {
                ExtractSyntax::From => format!("EXTRACT({field} FROM {expr})"),
                ExtractSyntax::Comma => format!("EXTRACT({field}, {expr})"),
            }
        }
        Expr::Ceil { expr, field } => rounding_text("CEIL", expr, field, depth, context)?,
        Expr::Floor { expr, field } => rounding_text("FLOOR", expr, field, depth, context)?,
        // `IN` could continue the first operand, so it is enclosed like any operand.
        Expr::Position { expr, r#in } => format!(
            "POSITION({} IN {})",
            normalize_expr_inner(expr, depth + 1, true, context)?,
            normalize_expr_inner(r#in, depth + 1, false, context)?
        ),
        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            special,
            shorthand,
        } => {
            let name = if *shorthand { "SUBSTR" } else { "SUBSTRING" };
            let (from, length) = if *special {
                (", ", ", ")
            } else {
                (" FROM ", " FOR ")
            };
            let mut text = format!(
                "{name}({}",
                normalize_expr_inner(expr, depth + 1, false, context)?
            );
            for (separator, part) in [(from, substring_from), (length, substring_for)] {
                if let Some(part) = part {
                    text.push_str(separator);
                    text.push_str(&normalize_expr_inner(part, depth + 1, false, context)?);
                }
            }
            text.push(')');
            text
        }
        Expr::Trim {
            expr,
            trim_where,
            trim_what,
            trim_characters,
        } => {
            let mut text = String::from("TRIM(");
            if let Some(trim_where) = trim_where {
                text.push_str(&format!("{trim_where} "));
            }
            if let Some(trim_what) = trim_what {
                text.push_str(&normalize_expr_inner(trim_what, depth + 1, false, context)?);
                text.push_str(" FROM ");
            }
            text.push_str(&normalize_expr_inner(expr, depth + 1, false, context)?);
            if let Some(characters) = trim_characters {
                text.push_str(", ");
                text.push_str(&list_text(characters, depth, context)?);
            }
            text.push(')');
            text
        }
        Expr::Overlay {
            expr,
            overlay_what,
            overlay_from,
            overlay_for,
        } => {
            let mut text = format!(
                "OVERLAY({} PLACING {} FROM {}",
                normalize_expr_inner(expr, depth + 1, false, context)?,
                normalize_expr_inner(overlay_what, depth + 1, false, context)?,
                normalize_expr_inner(overlay_from, depth + 1, false, context)?
            );
            if let Some(overlay_for) = overlay_for {
                text.push_str(" FOR ");
                text.push_str(&normalize_expr_inner(
                    overlay_for,
                    depth + 1,
                    false,
                    context,
                )?);
            }
            text.push(')');
            text
        }
        Expr::Case {
            case_token: _,
            end_token: _,
            operand,
            conditions,
            else_result,
        } => {
            let mut text = String::from("CASE");
            if let Some(operand) = operand {
                text.push(' ');
                text.push_str(&normalize_expr_inner(operand, depth + 1, false, context)?);
            }
            for when in conditions {
                text.push_str(" WHEN ");
                text.push_str(&normalize_expr_inner(
                    &when.condition,
                    depth + 1,
                    false,
                    context,
                )?);
                text.push_str(" THEN ");
                text.push_str(&normalize_expr_inner(
                    &when.result,
                    depth + 1,
                    false,
                    context,
                )?);
            }
            // `ELSE NULL` is what a missing `ELSE` yields.
            if let Some(result) = else_result.as_deref().filter(|result| !is_null(result)) {
                text.push_str(" ELSE ");
                text.push_str(&normalize_expr_inner(result, depth + 1, false, context)?);
            }
            text.push_str(" END");
            text
        }
        Expr::Tuple(items) => format!("({})", list_text(items, depth, context)?),
        Expr::Array(array) => {
            let keyword = if array.named { "ARRAY" } else { "" };
            format!("{keyword}[{}]", list_text(&array.elem, depth, context)?)
        }
        Expr::Interval(interval) => interval_text(interval, depth, context)?,
        Expr::TypedString(typed) => {
            if typed.uses_odbc_syntax {
                return Err(CanonicalizeError::Unsupported(
                    "ODBC escape literals are not supported".to_string(),
                ));
            }
            reject_lossy_quoting(&typed.value.value)?;
            format!("{} {}", typed.data_type, typed.value)
        }
        Expr::Prefixed { prefix, value } => format!(
            "{prefix} {}",
            normalize_expr_inner(value, depth + 1, true, context)?
        ),
        Expr::CompoundFieldAccess { root, access_chain } => {
            field_access_text(root, access_chain, depth, context)?
        }
        // Named statically, because printing an unbounded tree can be slow in sqlparser.
        Expr::Struct { .. } => return Err(refused("STRUCT")),
        Expr::Named { .. } => return Err(refused("a named expression")),
        Expr::Dictionary(_) => return Err(refused("a dictionary literal")),
        Expr::Map(_) => return Err(refused("a map literal")),
        Expr::Lambda(_) => return Err(refused("a lambda")),
        Expr::OuterJoin(_) => return Err(refused("the (+) outer join marker")),
        Expr::Prior(_) => return Err(refused("PRIOR")),
        Expr::GroupingSets(_) | Expr::Cube(_) | Expr::Rollup(_) => {
            return Err(refused("a grouping set"));
        }
        Expr::Wildcard(_) | Expr::QualifiedWildcard(..) => return Err(refused("a wildcard")),
    };
    Ok(if tight_parent && !encloses_itself(expr) {
        format!("({text})")
    } else {
        text
    })
}

/// How a binary operator treats the order of its operands.
#[derive(Clone, Copy)]
enum OperandOrder {
    /// The written order is part of the meaning.
    Written,
    /// Either order is the same predicate or value.
    Sorted,
    /// Either order is the same comparison where the operands' collation does not depend on
    /// their order.
    Compared,
    /// Swapping the operands takes the mirrored operator, as `a < b` is `b > a`.
    Mirrored(&'static str),
}

/// Spells `left operator right` in parentheses, so it reads back whole wherever it is nested,
/// with the operands in sorted order where `order` allows it.
fn infix_text(
    left: &Expr,
    operator: &'static str,
    right: &Expr,
    order: OperandOrder,
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    // Where the left operand's collation wins, as in SQLite, two operands may only swap when
    // one of them is a literal, which has no collation of its own.
    let swappable =
        context.collation_is_symmetric || is_literal(left, context) || is_literal(right, context);
    let left = normalize_expr_inner(left, depth + 1, true, context)?;
    let right = normalize_expr_inner(right, depth + 1, true, context)?;
    let (left, operator, right) = match order {
        OperandOrder::Sorted if left > right => (right, operator, left),
        OperandOrder::Compared if swappable && left > right => (right, operator, left),
        // Equal operands take whichever of the two operators sorts first, so `b > b` and
        // `b < b` share one spelling, which holds under any collation.
        OperandOrder::Mirrored(mirrored)
            if (swappable && left > right) || (left == right && mirrored < operator) =>
        {
            (right, mirrored, left)
        }
        _ => (left, operator, right),
    };
    Ok(format!("({left} {operator} {right})"))
}

/// Spells `operand suffix` for a postfix test such as `IS NULL`.
fn postfix_text(
    operand: &Expr,
    suffix: &str,
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let operand = normalize_expr_inner(operand, depth + 1, true, context)?;
    Ok(format!("{operand} {suffix}"))
}

/// Spells the keywords before `IN`, such as `GLOBAL NOT `, in sqlparser's order.
const fn membership_modifiers(global: bool, negated: bool) -> &'static str {
    match (global, negated) {
        (false, false) => "",
        (false, true) => "NOT ",
        (true, false) => "GLOBAL ",
        (true, true) => "GLOBAL NOT ",
    }
}

/// Spells the operator of a pattern match, such as `NOT SIMILAR TO`.
fn match_operator(negated: bool, operator: &str) -> String {
    if negated {
        format!("NOT {operator}")
    } else {
        operator.to_string()
    }
}

/// Spells `subject operator pattern`, with `ANY` before a pattern list and `ESCAPE` after the
/// pattern when the match has them.
fn pattern_match_text(
    subject: &Expr,
    operator: &str,
    any: bool,
    pattern: &Expr,
    escape: Option<&Expr>,
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let subject = normalize_expr_inner(subject, depth + 1, true, context)?;
    let pattern = if any {
        format!("ANY {}", argument_text(pattern, depth, context)?)
    } else {
        normalize_expr_inner(pattern, depth + 1, true, context)?
    };
    let escape = match escape {
        Some(escape) => format!(
            " ESCAPE {}",
            normalize_expr_inner(escape, depth + 1, true, context)?
        ),
        None => String::new(),
    };
    Ok(format!("{subject} {operator} {pattern}{escape}"))
}

/// Spells `left operator quantifier(right)` for `ANY` and `ALL`.
fn quantified_text(
    left: &Expr,
    operator: &BinaryOperator,
    quantifier: &str,
    right: &Expr,
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let left = normalize_expr_inner(left, depth + 1, true, context)?;
    let operator = operator_text(operator)?;
    let right = argument_text(right, depth, context)?;
    Ok(format!("{left} {operator} {quantifier}{right}"))
}

/// Spells the parenthesized argument of `ANY`, `ALL` or `LIKE ANY`. A tuple or a subquery
/// brings its own parentheses, so however many enclose it in the input, it gets one pair.
fn argument_text(
    argument: &Expr,
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let text = normalize_expr_inner(argument, depth + 1, false, context)?;
    let mut inner = argument;
    while let Expr::Nested(nested) = inner {
        inner = nested;
    }
    Ok(if matches!(inner, Expr::Tuple(_) | Expr::Subquery(_)) {
        text
    } else {
        format!("({text})")
    })
}

/// Refuses a call to a function named `ANY`, `SOME` or `ALL`.
///
/// Once ordering puts such a call on the right of a comparison, `1 = ANY(x)` reads back as a
/// quantified comparison, which is another predicate with the same text.
fn reject_quantifier_name(name: &ObjectName) -> Result<(), CanonicalizeError> {
    let quantifier = match name.0.as_slice() {
        [part] => part.as_ident().is_some_and(|ident| {
            ident.quote_style.is_none()
                && ["ANY", "SOME", "ALL"]
                    .iter()
                    .any(|keyword| ident.value.eq_ignore_ascii_case(keyword))
        }),
        _ => false,
    };
    if quantifier {
        Err(CanonicalizeError::Unsupported(format!(
            "Function named {name} reads back as a quantifier"
        )))
    } else {
        Ok(())
    }
}

/// Spells a dotted name, folding the last part as a column and the others as qualifiers.
fn qualified_name_text<'i>(
    parts: impl ExactSizeIterator<Item = &'i Ident>,
    place: NamePlace,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let column = parts.len().saturating_sub(1);
    Ok(parts
        .enumerate()
        .map(|(index, part)| {
            let folding = if index == column {
                context.folding
            } else {
                context.qualifier_folding
            };
            identifier_text(part, folding, place, context)
        })
        .collect::<Result<Vec<_>, _>>()?
        .join("."))
}

/// Moves to the head of a sorted `IN` list the first item that can stand there.
///
/// sqlparser reads a list that opens with a query as `IN (query)` and does not take that back.
/// Enclosing hides a query that only starts an item, as in `(SELECT a FROM u)[1]`, and a bare
/// subquery keeps its query first however many parentheses enclose it. Every other item keeps
/// its sorted place, and a list whose items all start with a query fails to read back.
fn open_list(items: &mut [String], context: &Canonicalizer<'_>) {
    if items
        .first()
        .is_none_or(|head| !starts_with_query(head, context))
    {
        return;
    }
    let head = items.iter_mut().position(|item| {
        if !starts_with_query(item, context) {
            return true;
        }
        let enclosed = format!("({item})");
        let can_lead = !starts_with_query(&enclosed, context);
        if can_lead {
            *item = enclosed;
        }
        can_lead
    });
    if let Some(head) = head {
        items[..=head].rotate_right(1);
    }
}

/// Reports whether sqlparser reads a query at the start of `text`, as it tries to at the start
/// of an `IN` list. A query starts with a parenthesis or a keyword, so any other text is
/// answered without parsing.
fn starts_with_query(text: &str, context: &Canonicalizer<'_>) -> bool {
    let first_word = text
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .next()
        .unwrap_or_default();
    (text.starts_with('(') || is_keyword(first_word))
        && Parser::new(context.dialect)
            .try_with_sql(text)
            .and_then(|mut parser| parser.parse_query())
            .is_ok()
}

/// The error for an expression form no `WHERE` clause of a served query can hold.
fn refused(form: &str) -> CanonicalizeError {
    CanonicalizeError::Unsupported(format!("{form} is not supported in a predicate"))
}

/// Spells a subquery in the shape the crate serves, `SELECT items FROM table [WHERE filter]`.
///
/// Inside a subquery, grouping, ordering and column aliases change which rows the outer
/// predicate sees, so they are refused along with every clause an outer query may not carry.
/// A table alias only names the table for the qualifiers that follow, so it is kept, folded as
/// a qualifier and written without `AS`, which Oracle refuses there. `ONLY` keeps descendant
/// tables out of the rows read, so it is kept, while `u *` reads what a bare `u` reads and is
/// written as `u`.
fn subquery_text(
    query: &Query,
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let unsupported = |part: &str| {
        CanonicalizeError::Unsupported(format!("{part} in a subquery is not supported"))
    };
    let select = single_table_select(query)?;
    if query.order_by.is_some() {
        return Err(unsupported("ORDER BY"));
    }
    if !matches!(&select.group_by, GroupByExpr::Expressions(exprs, modifiers) if exprs.is_empty() && modifiers.is_empty())
    {
        return Err(unsupported("GROUP BY"));
    }
    if select.having.is_some() {
        return Err(unsupported("HAVING"));
    }
    let TableFactor::Table {
        name,
        alias,
        args: None,
        with_hints,
        version: None,
        with_ordinality: false,
        partitions,
        json_path: None,
        sample: None,
        index_directives,
        inheritance,
    } = &select.from[0].relation
    else {
        return Err(unsupported("a table option"));
    };
    if !with_hints.is_empty() || !partitions.is_empty() || !index_directives.is_empty() {
        return Err(unsupported("a table hint"));
    }
    let items = select
        .projection
        .iter()
        .map(|item| match item {
            SelectItem::UnnamedExpr(expr) => normalize_expr_inner(expr, depth + 1, false, context),
            SelectItem::Wildcard(options)
                if options.opt_ilike.is_none()
                    && options.opt_exclude.is_none()
                    && options.opt_except.is_none()
                    && options.opt_replace.is_none()
                    && options.opt_rename.is_none()
                    && options.opt_alias.is_none() =>
            {
                Ok("*".to_string())
            }
            _ => Err(unsupported("an alias or qualified wildcard")),
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let table = name
        .0
        .iter()
        .map(|part| match part.as_ident() {
            Some(part) => {
                identifier_text(part, context.qualifier_folding, NamePlace::Operand, context)
            }
            None => Err(unsupported("a computed table name")),
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(".");
    let only = match inheritance {
        Some(TableInheritance::Only { parenthesized: _ }) => "ONLY ",
        Some(TableInheritance::IncludeDescendants) | None => "",
    };
    let mut text = format!("SELECT {items} FROM {only}{table}");
    if let Some(TableAlias {
        explicit: _,
        name,
        columns,
        at,
    }) = alias
    {
        if !columns.is_empty() || at.is_some() {
            return Err(unsupported("a table alias with columns or an index"));
        }
        text.push(' ');
        text.push_str(&identifier_text(
            name,
            context.qualifier_folding,
            NamePlace::Operand,
            context,
        )?);
    }
    // `WHERE TRUE` keeps every row, as a missing `WHERE` does, where `TRUE` is reserved.
    if let Some(filter) = select
        .selection
        .as_ref()
        .filter(|filter| !(context.true_is_reserved && is_true(filter)))
    {
        text.push_str(" WHERE ");
        text.push_str(&normalize_expr_inner(filter, depth + 1, false, context)?);
    }
    Ok(text)
}

/// Spells a plain call, refusing the aggregate and window forms no `WHERE` clause can hold.
fn function_text(
    function: &Function,
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let Function {
        name,
        uses_odbc_syntax,
        parameters,
        args,
        filter,
        null_treatment,
        over,
        within_group,
    } = function;
    reject_quantifier_name(name)?;
    let plain = !*uses_odbc_syntax
        && matches!(parameters, FunctionArguments::None)
        && filter.is_none()
        && null_treatment.is_none()
        && over.is_none()
        && within_group.is_empty();
    let unsupported =
        || CanonicalizeError::Unsupported(format!("Call to {name} is not a plain function call"));
    if !plain {
        return Err(unsupported());
    }
    let name = object_name_text(name, NamePlace::Function, context)?;
    match args {
        FunctionArguments::None => Ok(name),
        FunctionArguments::Subquery(_) => Err(unsupported()),
        FunctionArguments::List(list) => {
            if list.duplicate_treatment.is_some() || !list.clauses.is_empty() {
                return Err(unsupported());
            }
            let mut text = name;
            text.push('(');
            append_function_args(&mut text, &list.args, depth, context)?;
            text.push(')');
            Ok(text)
        }
    }
}

/// Encloses predicate arguments so they cannot read back as special call syntax.
fn append_function_args(
    text: &mut String,
    args: &[FunctionArg],
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<(), CanonicalizeError> {
    for (index, arg) in args.iter().enumerate() {
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(arg)) = arg else {
            return Err(refused("a named or wildcard function argument"));
        };
        if index > 0 {
            text.push_str(", ");
        }
        text.push_str(&normalize_expr_inner(arg, depth + 1, true, context)?);
    }
    Ok(())
}

/// Spells a comma separated list of expressions in their written order.
fn list_text(
    items: &[Expr],
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    Ok(items
        .iter()
        .map(|item| normalize_expr_inner(item, depth + 1, false, context))
        .collect::<Result<Vec<_>, _>>()?
        .join(", "))
}

/// Spells `CEIL` or `FLOOR` with its optional date part or scale.
fn rounding_text(
    name: &str,
    expr: &Expr,
    field: &CeilFloorKind,
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let expr = normalize_expr_inner(expr, depth + 1, false, context)?;
    Ok(match field {
        CeilFloorKind::DateTimeField(DateTimeField::NoDateTime) => format!("{name}({expr})"),
        CeilFloorKind::DateTimeField(field) => format!("{name}({expr} TO {field})"),
        CeilFloorKind::Scale(scale) => format!("{name}({expr}, {scale})"),
    })
}

/// Spells an interval with its value normalized and its qualifiers as sqlparser writes them.
fn interval_text(
    interval: &Interval,
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let Interval {
        value,
        leading_field,
        leading_precision,
        last_field,
        fractional_seconds_precision,
    } = interval;
    let mut text = format!(
        "INTERVAL {}",
        normalize_expr_inner(value, depth + 1, true, context)?
    );
    if let (Some(DateTimeField::Second), Some(leading), Some(fractional)) = (
        leading_field,
        leading_precision,
        fractional_seconds_precision,
    ) {
        text.push_str(&format!(" SECOND ({leading}, {fractional})"));
        return Ok(text);
    }
    if let Some(leading_field) = leading_field {
        text.push_str(&format!(" {leading_field}"));
    }
    if let Some(leading_precision) = leading_precision {
        text.push_str(&format!(" ({leading_precision})"));
    }
    if let Some(last_field) = last_field {
        text.push_str(&format!(" TO {last_field}"));
    }
    if let Some(fractional) = fractional_seconds_precision {
        text.push_str(&format!(" ({fractional})"));
    }
    Ok(text)
}

/// Spells a field or subscript access.
///
/// Parentheses around the root decide what it is, since `(c).f` reads a field of the value
/// `c` while `c.f` may read column `f` of table `c`, so a parenthesized root keeps one pair.
/// A bare name in the access may be a table, so it folds with the qualifier rule.
fn field_access_text(
    root: &Expr,
    access_chain: &[AccessExpr],
    depth: usize,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let mut text = match root {
        Expr::Nested(inner) => format!(
            "({})",
            normalize_expr_inner(inner, depth + 1, false, context)?
        ),
        Expr::Identifier(name) => {
            identifier_text(name, context.qualifier_folding, NamePlace::Operand, context)?
        }
        Expr::Value(_) => return Err(literal_access()),
        root => normalize_expr_inner(root, depth + 1, true, context)?,
    };
    for access in access_chain {
        match access {
            AccessExpr::Dot(Expr::Identifier(name)) => {
                text.push('.');
                text.push_str(&identifier_text(
                    name,
                    context.qualifier_folding,
                    NamePlace::Operand,
                    context,
                )?);
            }
            AccessExpr::Dot(Expr::Value(_)) => return Err(literal_access()),
            AccessExpr::Dot(field) => {
                text.push('.');
                text.push_str(&normalize_expr_inner(field, depth + 1, true, context)?);
            }
            AccessExpr::Subscript(Subscript::Index { index }) => {
                text.push('[');
                text.push_str(&normalize_expr_inner(index, depth + 1, false, context)?);
                text.push(']');
            }
            AccessExpr::Subscript(Subscript::Slice {
                lower_bound,
                upper_bound,
                stride,
            }) => {
                text.push('[');
                if let Some(lower_bound) = lower_bound {
                    text.push_str(&normalize_expr_inner(
                        lower_bound,
                        depth + 1,
                        false,
                        context,
                    )?);
                }
                text.push(':');
                if let Some(upper_bound) = upper_bound {
                    text.push_str(&normalize_expr_inner(
                        upper_bound,
                        depth + 1,
                        false,
                        context,
                    )?);
                }
                if let Some(stride) = stride {
                    text.push(':');
                    text.push_str(&normalize_expr_inner(stride, depth + 1, false, context)?);
                }
                text.push(']');
            }
        }
    }
    Ok(text)
}

/// The error for a field access on a literal or naming a field by one. Printed, `0 .l` reads
/// `0.l`, which a tokenizer splits into the number `0.` and a name.
fn literal_access() -> CanonicalizeError {
    CanonicalizeError::Unsupported("A field access on a literal is not supported".to_string())
}

/// Reports whether `expr` is a literal, signed or not, however many parentheses enclose it.
///
/// A boolean counts only where `TRUE` is reserved, because SQLite reads `TRUE` and `FALSE` as
/// columns of those names when the table has them.
fn is_literal(expr: &Expr, context: &Canonicalizer<'_>) -> bool {
    match expr {
        Expr::Nested(inner) => is_literal(inner, context),
        Expr::UnaryOp {
            op: UnaryOperator::Minus | UnaryOperator::Plus,
            expr,
        } => is_literal(expr, context),
        Expr::Value(value) => context.true_is_reserved || !matches!(value.value, Value::Boolean(_)),
        _ => false,
    }
}

/// Reports whether `expr` is `TRUE`, however many parentheses enclose it.
fn is_true(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => is_true(inner),
        Expr::Value(value) => matches!(value.value, Value::Boolean(true)),
        _ => false,
    }
}

/// Reports whether `expr` is `NULL`, however many parentheses enclose it.
fn is_null(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => is_null(inner),
        Expr::Value(value) => matches!(value.value, Value::Null),
        _ => false,
    }
}

/// Spells an object name that names a column, refusing a part computed by a function.
fn object_name_text(
    name: &ObjectName,
    place: NamePlace,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let parts = name
        .0
        .iter()
        .map(|part| {
            part.as_ident().ok_or_else(|| {
                CanonicalizeError::Unsupported(format!("Computed name part: {part}"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    qualified_name_text(parts.into_iter(), place, context)
}

/// Reports whether `expr` prints as text no neighbouring operator can split.
///
/// Names, literals, forms with delimiters of their own such as calls and `CASE`, and prefix
/// signs qualify. Every other form is enclosed in parentheses as an operand, because how
/// tightly it binds differs by dialect and its text could read back grouped another way,
/// which can give two predicates one key. The match names every variant, so a variant a later
/// sqlparser adds is classified before the crate builds.
const fn encloses_itself(expr: &Expr) -> bool {
    match expr {
        // NOT binds looser than any operator that can enclose it.
        Expr::UnaryOp { op, .. } => !matches!(op, UnaryOperator::Not),
        Expr::Exists { negated, .. } => !*negated,
        Expr::Identifier(_)
        | Expr::CompoundIdentifier(_)
        | Expr::CompoundFieldAccess { .. }
        | Expr::Value(_)
        | Expr::TypedString(_)
        | Expr::Prefixed { .. }
        | Expr::Nested(_)
        | Expr::BinaryOp { .. }
        | Expr::IsDistinctFrom(..)
        | Expr::IsNotDistinctFrom(..)
        | Expr::Function(_)
        | Expr::Cast { .. }
        | Expr::Convert { .. }
        | Expr::Extract { .. }
        | Expr::Ceil { .. }
        | Expr::Floor { .. }
        | Expr::Position { .. }
        | Expr::Substring { .. }
        | Expr::Trim { .. }
        | Expr::Overlay { .. }
        | Expr::Case { .. }
        | Expr::Subquery(_)
        | Expr::Tuple(_)
        | Expr::Array(_)
        | Expr::MatchAgainst { .. }
        | Expr::Wildcard(_)
        | Expr::QualifiedWildcard(..) => true,
        Expr::IsNull(_)
        | Expr::IsNotNull(_)
        | Expr::IsTrue(_)
        | Expr::IsNotTrue(_)
        | Expr::IsFalse(_)
        | Expr::IsNotFalse(_)
        | Expr::IsUnknown(_)
        | Expr::IsNotUnknown(_)
        | Expr::IsJson { .. }
        | Expr::IsNormalized { .. }
        | Expr::In { .. }
        | Expr::Between { .. }
        | Expr::Like { .. }
        | Expr::ILike { .. }
        | Expr::SimilarTo { .. }
        | Expr::RLike { .. }
        | Expr::AnyOp { .. }
        | Expr::AllOp { .. }
        | Expr::MemberOf(_)
        | Expr::AtTimeZone { .. }
        | Expr::Collate { .. }
        | Expr::Interval(_)
        | Expr::JsonAccess { .. }
        | Expr::GroupingSets(_)
        | Expr::Cube(_)
        | Expr::Rollup(_)
        | Expr::Struct { .. }
        | Expr::Named { .. }
        | Expr::Dictionary(_)
        | Expr::Map(_)
        | Expr::Lambda(_)
        | Expr::OuterJoin(_)
        | Expr::Prior(_) => false,
    }
}

/// How a dialect decides whether two spellings name the same column or table.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Folding {
    /// An unquoted name folds to lower case, a quoted one keeps its spelling.
    LowerUnquoted,
    /// An unquoted name folds to upper case, a quoted one keeps its spelling.
    UpperUnquoted,
    /// Every spelling of the same letters is one name.
    CaseInsensitive,
    /// Unknown rule, so keep the name exactly as written and never merge two spellings.
    Exact,
}

/// Canonicalizes predicates under one SQL dialect.
///
/// The dialect decides how identifiers fold, so it decides which two spellings mean one
/// column. Build this from the dialect that parsed whatever you hand it.
pub struct Canonicalizer<'a> {
    dialect: &'a dyn Dialect,
    folding: Folding,
    qualifier_folding: Folding,
    /// Whether a bare `TRUE` is always the boolean, and never a column named `true`.
    true_is_reserved: bool,
    /// Whether `+` only adds numbers, so `a + b` and `b + a` are one value.
    plus_is_numeric: bool,
    /// Whether comparing two operands uses the same collation in either order.
    collation_is_symmetric: bool,
}

impl<'a> Canonicalizer<'a> {
    /// Reads the folding rules for `dialect`.
    #[must_use]
    pub fn new(dialect: &'a dyn Dialect) -> Self {
        let folding = if dialect.is::<PostgreSqlDialect>() {
            Folding::LowerUnquoted
        } else if dialect.is::<AnsiDialect>() {
            Folding::UpperUnquoted
        } else if dialect.is::<MySqlDialect>() || dialect.is::<SQLiteDialect>() {
            Folding::CaseInsensitive
        } else {
            Folding::Exact
        };
        // MySQL matches table and database names by case unless the server sets
        // `lower_case_table_names`, so folding them could merge two tables.
        let qualifier_folding = if dialect.is::<MySqlDialect>() {
            Folding::Exact
        } else {
            folding
        };
        let true_is_reserved = dialect.is::<PostgreSqlDialect>()
            || dialect.is::<MySqlDialect>()
            || dialect.is::<AnsiDialect>();
        let plus_is_numeric = true_is_reserved || dialect.is::<SQLiteDialect>();
        // PostgreSQL, MySQL and ANSI reject two conflicting implicit collations in either order,
        // while SQLite takes the left operand's.
        let collation_is_symmetric = true_is_reserved;
        Self {
            dialect,
            folding,
            qualifier_folding,
            true_is_reserved,
            plus_is_numeric,
            collation_is_symmetric,
        }
    }
}

/// Where a name stands in canonical text.
#[derive(Clone, Copy, Eq, PartialEq)]
enum NamePlace {
    /// A column, table or field name, which a keyword spelling can turn into an expression.
    Operand,
    /// A function name, which keeps whether it was quoted, because MySQL looks a quoted
    /// function name up among stored functions and not among its own.
    Function,
}

/// Resolves an identifier to the name the database would see, then spells it the one way
/// that reads back as that same name.
fn identifier_text(
    identifier: &Ident,
    folding: Folding,
    place: NamePlace,
    context: &Canonicalizer<'_>,
) -> Result<String, CanonicalizeError> {
    let quoted = identifier.quote_style.is_some();
    // Case folding rules are stated for ASCII. Anything else keeps its exact spelling,
    // because merging two names the database might separate is the unsafe direction.
    let folding = if identifier.value.is_ascii() {
        folding
    } else {
        Folding::Exact
    };
    let resolved = match folding {
        Folding::LowerUnquoted if !quoted => identifier.value.to_ascii_lowercase(),
        Folding::UpperUnquoted if !quoted => identifier.value.to_ascii_uppercase(),
        Folding::CaseInsensitive => identifier.value.to_ascii_lowercase(),
        _ => identifier.value.clone(),
    };

    if bare_spelling_is_faithful(&resolved, quoted, folding, place) {
        return Ok(resolved);
    }
    // Where no folding rule ties the two spellings, quoting a bare name could name another
    // column, so a bare name that cannot stay bare is refused.
    if !quoted && folding == Folding::Exact {
        return Err(CanonicalizeError::Unsupported(format!(
            "Name {resolved} reads as a keyword out of its place"
        )));
    }
    // A delimited name escapes its own delimiter by doubling it, so `a"b` is written
    // `"a""b"`. Emitting the delimiter raw produces a name that reads back as something else.
    let quote = identifier
        .quote_style
        .or_else(|| context.dialect.identifier_quote_style(&resolved))
        .unwrap_or('"');
    let mut delimited = String::with_capacity(resolved.len() + 2);
    delimited.push(quote);
    for character in resolved.chars() {
        if character == quote {
            delimited.push(quote);
        }
        delimited.push(character);
    }
    delimited.push(quote);
    Ok(delimited)
}

/// Reports whether writing `name` without quotes reads back as `name` itself.
///
/// The answer must depend only on the name, never on how canonicalization is going, or one
/// predicate could end up with two spellings and so two keys.
fn bare_spelling_is_faithful(name: &str, quoted: bool, folding: Folding, place: NamePlace) -> bool {
    if !quoted {
        // Folding only applied the change the dialect makes itself, so the bare name is the
        // same name as long as it reads as a name wherever canonicalization puts it. A call
        // keeps its name, because it is only a call once its name failed as special syntax,
        // and its enclosed arguments fail that syntax again.
        return place == NamePlace::Function || !starts_expression_syntax(name);
    }
    // A quoted function name stays quoted, because MySQL looks it up among stored functions.
    if place == NamePlace::Function || !is_plain_non_keyword(name) {
        return false;
    }
    match folding {
        // Dropping the quotes hands the name to the dialect's folding, so it may only go bare
        // when it is already in the folded form.
        Folding::LowerUnquoted => !name.bytes().any(|byte| byte.is_ascii_uppercase()),
        Folding::UpperUnquoted => !name.bytes().any(|byte| byte.is_ascii_lowercase()),
        Folding::CaseInsensitive => true,
        Folding::Exact => false,
    }
}

/// Reports whether `name` is spelled like a keyword of any dialect.
fn is_keyword(name: &str) -> bool {
    let upper = || name.bytes().map(|byte| byte.to_ascii_uppercase());
    ALL_KEYWORDS
        .binary_search_by(|keyword| keyword.bytes().cmp(upper()))
        .is_ok()
}

/// Words sqlparser reads as the start of an expression, taking one as a name only when the
/// expression it starts fails to parse. Such a name, like `NOT` inside `POSITION(NOT - x IN
/// y)`, reads as the keyword again once canonicalization moves it. The list is the one
/// `parse_expr_prefix_by_reserved_word` handles in sqlparser 0.63.
const EXPRESSION_KEYWORDS: &[&str] = &[
    "ARRAY",
    "BOX",
    "CASE",
    "CAST",
    "CEIL",
    "CIRCLE",
    "CONVERT",
    "CURRENT_CATALOG",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "EXISTS",
    "EXTRACT",
    "FALSE",
    "FLOOR",
    "INTERVAL",
    "LAMBDA",
    "LINE",
    "LOCALTIME",
    "LOCALTIMESTAMP",
    "LSEG",
    "MAP",
    "MATCH",
    "NOT",
    "NULL",
    "OVERLAY",
    "PATH",
    "POINT",
    "POLYGON",
    "POSITION",
    "PRIOR",
    "SAFE_CAST",
    "SELECT",
    "SESSION_USER",
    "STRUCT",
    "SUBSTR",
    "SUBSTRING",
    "TRIM",
    "TRUE",
    "TRY_CAST",
    "TRY_CONVERT",
    "USER",
    "WITH",
];

/// Reports whether sqlparser may read `name` written bare as the start of an expression.
fn starts_expression_syntax(name: &str) -> bool {
    EXPRESSION_KEYWORDS
        .iter()
        .any(|keyword| keyword.eq_ignore_ascii_case(name))
}

/// Reports whether `name` is a bare word no dialect reads as a keyword.
///
/// Whether a given keyword may still name a column is dialect and position specific, so this
/// errs towards keeping the quotes. The cost is that a quoted keyword does not share a key
/// with its unquoted spelling, which splits one column into two entries rather than merging
/// two columns into one.
fn is_plain_non_keyword(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_')
        || !characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return false;
    }
    !is_keyword(name)
}

/// Rejects a literal the parser cannot print without changing its value.
fn reject_lossy_quoting(value: &Value) -> Result<(), CanonicalizeError> {
    let intact = match value {
        Value::SingleQuotedString(text) | Value::NationalStringLiteral(text) => {
            quoting_survives_printing(text, '\'')
        }
        Value::DoubleQuotedString(text) => quoting_survives_printing(text, '"'),
        _ => true,
    };
    if intact {
        Ok(())
    } else {
        Err(CanonicalizeError::NotRoundTrippable(
            "string literal".to_string(),
        ))
    }
}

/// Reports whether printing `text` inside `quote` delimiters preserves it.
///
/// `sqlparser` leaves a quote alone when it already looks escaped, either doubled or preceded
/// by a backslash, so such text reads back one escape level shorter than it went in.
fn quoting_survives_printing(text: &str, quote: char) -> bool {
    let mut characters = text.chars().peekable();
    let mut previous = char::default();
    while let Some(character) = characters.next() {
        if character == quote
            && (previous == '\\' || characters.peek().is_some_and(|next| *next == quote))
        {
            return false;
        }
        previous = character;
    }
    true
}

fn collect_flat_children<'a>(expr: &'a Expr, operator: &BinaryOperator) -> Vec<&'a Expr> {
    match expr {
        Expr::Nested(inner) => collect_flat_children(inner, operator),
        Expr::BinaryOp { left, op, right } if op == operator => {
            let mut children = collect_flat_children(left, operator);
            children.extend(collect_flat_children(right, operator));
            children
        }
        _ => vec![expr],
    }
}

/// Reads how `operator` treats the order of its operands under the canonicalizer's dialect.
///
/// `AND` and `OR` never reach here, because their chains are flattened and sorted whole.
const fn operand_order(operator: &BinaryOperator, context: &Canonicalizer<'_>) -> OperandOrder {
    match operator {
        BinaryOperator::Eq
        | BinaryOperator::NotEq
        | BinaryOperator::Spaceship
        | BinaryOperator::Is
        | BinaryOperator::IsNot => OperandOrder::Compared,
        // SQL Server also concatenates strings with `+`, where the order matters.
        BinaryOperator::Plus | BinaryOperator::Multiply if context.plus_is_numeric => {
            OperandOrder::Sorted
        }
        BinaryOperator::Lt => OperandOrder::Mirrored(">"),
        BinaryOperator::Gt => OperandOrder::Mirrored("<"),
        BinaryOperator::LtEq => OperandOrder::Mirrored(">="),
        BinaryOperator::GtEq => OperandOrder::Mirrored("<="),
        _ => OperandOrder::Written,
    }
}

fn operator_text(operator: &BinaryOperator) -> Result<&'static str, CanonicalizeError> {
    match operator {
        BinaryOperator::And => Ok("AND"),
        BinaryOperator::Or => Ok("OR"),
        BinaryOperator::Eq => Ok("="),
        BinaryOperator::Spaceship => Ok("<=>"),
        BinaryOperator::NotEq => Ok("!="),
        BinaryOperator::Lt => Ok("<"),
        BinaryOperator::LtEq => Ok("<="),
        BinaryOperator::Gt => Ok(">"),
        BinaryOperator::GtEq => Ok(">="),
        BinaryOperator::Plus => Ok("+"),
        BinaryOperator::Minus => Ok("-"),
        BinaryOperator::Multiply => Ok("*"),
        BinaryOperator::Divide => Ok("/"),
        BinaryOperator::Modulo => Ok("%"),
        // SQLite also spells `IS NOT DISTINCT FROM` as `IS`, and `IS DISTINCT FROM` as `IS NOT`.
        BinaryOperator::Is => Ok("IS NOT DISTINCT FROM"),
        BinaryOperator::IsNot => Ok("IS DISTINCT FROM"),
        other => Err(CanonicalizeError::Unsupported(format!(
            "Unsupported binary operator: {other}"
        ))),
    }
}

fn unary_operator_text(operator: &UnaryOperator) -> Result<&'static str, CanonicalizeError> {
    match operator {
        UnaryOperator::Not => Ok("NOT"),
        UnaryOperator::Plus => Ok("+"),
        UnaryOperator::Minus => Ok("-"),
        other => Err(CanonicalizeError::Unsupported(format!(
            "Unsupported unary operator: {other}"
        ))),
    }
}
