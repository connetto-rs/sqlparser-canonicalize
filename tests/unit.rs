use sqlparser::ast::helpers::attached_token::AttachedToken;
use sqlparser::ast::{Expr, Ident, SetExpr, Statement};
use sqlparser::dialect::{
    AnsiDialect, BigQueryDialect, ClickHouseDialect, DatabricksDialect, Dialect, DuckDbDialect,
    GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect, SnowflakeDialect,
};
use sqlparser::parser::Parser;
use sqlparser_canonicalize::{CanonicalizeError, Canonicalizer, hash_canonical};

#[test]
fn test_normalize_simple() {
    let dialect = PostgreSqlDialect {};

    let sql = "SELECT * FROM t WHERE age > 18";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql);
    assert!(result.is_ok());

    assert_eq!(result.as_deref(), Ok("(18 < age)"));
}

#[test]
fn test_normalize_commutative_and() {
    let dialect = PostgreSqlDialect {};

    let sql1 = "SELECT * FROM t WHERE a = 1 AND b = 2";
    let sql2 = "SELECT * FROM t WHERE b = 2 AND a = 1";

    let norm1 = Canonicalizer::new(&dialect).normalize_sql(sql1).unwrap();
    let norm2 = Canonicalizer::new(&dialect).normalize_sql(sql2).unwrap();

    assert_eq!(norm1, norm2);
}

#[test]
fn test_normalize_commutative_or() {
    let dialect = PostgreSqlDialect {};

    let sql1 = "SELECT * FROM t WHERE a = 1 OR b = 2";
    let sql2 = "SELECT * FROM t WHERE b = 2 OR a = 1";

    let norm1 = Canonicalizer::new(&dialect).normalize_sql(sql1).unwrap();
    let norm2 = Canonicalizer::new(&dialect).normalize_sql(sql2).unwrap();

    assert_eq!(norm1, norm2);
}

#[test]
fn test_normalize_in_list_sorted() {
    let dialect = PostgreSqlDialect {};

    let sql1 = "SELECT * FROM t WHERE x IN (1, 2, 3)";
    let sql2 = "SELECT * FROM t WHERE x IN (3, 1, 2)";

    let norm1 = Canonicalizer::new(&dialect).normalize_sql(sql1).unwrap();
    let norm2 = Canonicalizer::new(&dialect).normalize_sql(sql2).unwrap();

    assert_eq!(norm1, norm2);
}

#[test]
fn a_membership_term_normalizes_the_same_under_two_spellings() {
    let dialect = PostgreSqlDialect {};

    let one = Canonicalizer::new(&dialect)
        .normalize_sql("SELECT * FROM t WHERE x IN (SELECT id FROM m WHERE owner = 'a')")
        .unwrap();
    let two = Canonicalizer::new(&dialect)
        .normalize_sql(
            "SELECT   *  FROM t\n  where   x   in   ( select id from m where owner = 'a' )",
        )
        .unwrap();

    assert_eq!(one, two, "one filter, two spellings, one predicate");
    assert!(
        !one.contains("Span") && !one.contains("Ident"),
        "the term must not normalize through the Debug fallback, got {one:?}"
    );
}

#[test]
fn two_different_membership_terms_are_two_predicates() {
    let dialect = PostgreSqlDialect {};
    let norm = |sql: &str| Canonicalizer::new(&dialect).normalize_sql(sql);

    let base = norm("SELECT * FROM t WHERE x IN (SELECT id FROM m WHERE owner = 'a')");
    assert!(base.is_ok());

    for other in [
        "SELECT * FROM t WHERE x IN (SELECT id FROM n WHERE owner = 'a')",
        "SELECT * FROM t WHERE x IN (SELECT ref FROM m WHERE owner = 'a')",
        "SELECT * FROM t WHERE x IN (SELECT id FROM m WHERE owner = 'b')",
        "SELECT * FROM t WHERE y IN (SELECT id FROM m WHERE owner = 'a')",
        "SELECT * FROM t WHERE x IN (SELECT id FROM m WHERE owner = 'a' LIMIT 1)",
        "SELECT * FROM t WHERE x NOT IN (SELECT id FROM m WHERE owner = 'a')",
    ] {
        assert_ne!(
            base,
            norm(other),
            "{other} names a different relationship and must not share the predicate"
        );
    }
}

#[test]
fn a_plain_membership_subquery_is_canonicalized() {
    for dialect in [&MySqlDialect {} as &dyn Dialect, &SQLiteDialect {}] {
        let result = Canonicalizer::new(dialect)
            .normalize_sql("SELECT * FROM orders WHERE id IN (SELECT id FROM items)");
        assert_eq!(result.as_deref(), Ok("id IN (SELECT id FROM items)"));
    }
}

#[test]
fn table_membership_normalizes_names_and_operands() {
    assert_canonical(
        &SQLiteDialect {},
        &[
            ("X IN Widgets", "x IN widgets"),
            ("x in \"WIDGETS\"", "x IN widgets"),
            ("X NOT IN Main.Widgets", "x NOT IN main.widgets"),
            ("(A = 1) IN [Db].[Widgets]", "(1 = a) IN db.widgets"),
            ("x IN \"order\"", "x IN \"order\""),
            ("x IN \"odd\"\"name\"", "x IN \"odd\"\"name\""),
        ],
    );
    assert_canonical(
        &GenericDialect {},
        &[("X IN Main.Items", "X IN Main.Items")],
    );
}

#[test]
fn table_membership_keeps_function_arguments_in_order() {
    assert_canonical(
        &SQLiteDialect {},
        &[
            ("X IN Main.F(A = 1, (B))", "x IN main.f((1 = a), b)"),
            ("x NOT IN F(B, A = 1)", "x NOT IN f(b, (1 = a))"),
            ("x IN F()", "x IN f()"),
            ("x IN F", "x IN f"),
            ("x IN F(1, 2)", "x IN f(1, 2)"),
            ("x IN F(2, 1)", "x IN f(2, 1)"),
            ("x IN F('a', NULL)", "x IN f('a', NULL)"),
            ("x IN F(x IN items)", "x IN f((x IN items))"),
        ],
    );
}

#[test]
fn table_membership_refuses_non_expression_arguments() {
    assert_unsupported(
        &SQLiteDialect {},
        &["x IN f(*)", "x IN f(t.*)", "x IN f(name => a)"],
    );
}

#[test]
fn subquery_index_directives_are_refused() {
    for (dialect, directives) in [
        (
            &MySqlDialect {} as &dyn Dialect,
            &["USE INDEX (idx)", "FORCE INDEX (idx)", "IGNORE INDEX (idx)"][..],
        ),
        (&SQLiteDialect {}, &["INDEXED BY idx", "NOT INDEXED"][..]),
    ] {
        for directive in directives {
            let sql =
                format!("SELECT * FROM orders WHERE id IN (SELECT id FROM items {directive})");
            let result = Canonicalizer::new(dialect).normalize_sql(&sql);
            assert!(
                matches!(result, Err(CanonicalizeError::Unsupported(_))),
                "{sql} must refuse the index directive, got {result:?}"
            );
        }
    }
}

#[test]
fn test_normalize_no_where() {
    let dialect = PostgreSqlDialect {};

    let sql = "SELECT * FROM t";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql);
    assert!(result.is_ok());

    let normalized = result.unwrap();
    assert_eq!(normalized, "true");
}

#[test]
fn test_hash_deterministic() {
    let s = "age > 18 AND status = 'active'";

    let hash1 = hash_canonical(s);
    let hash2 = hash_canonical(s);

    assert_eq!(hash1, hash2);
}

#[test]
fn test_hash_different() {
    let s1 = "age > 18";
    let s2 = "age > 19";

    let hash1 = hash_canonical(s1);
    let hash2 = hash_canonical(s2);

    assert_ne!(hash1, hash2);
}

#[test]
fn test_hash_128bit() {
    let s = "test";
    let hash = hash_canonical(s);

    assert!(hash > 0);
    assert!(hash < u128::MAX);
}

#[test]
fn test_normalize_nested_parentheses() {
    let dialect = PostgreSqlDialect {};

    let sql1 = "SELECT * FROM t WHERE ((age > 18))";
    let sql2 = "SELECT * FROM t WHERE age > 18";

    let norm1 = Canonicalizer::new(&dialect).normalize_sql(sql1).unwrap();
    let norm2 = Canonicalizer::new(&dialect).normalize_sql(sql2).unwrap();

    assert_eq!(norm1, norm2);
}

#[test]
fn test_normalize_preserves_order_noncommutative() {
    let dialect = PostgreSqlDialect {};

    let sql1 = "SELECT * FROM t WHERE a < b";
    let sql2 = "SELECT * FROM t WHERE b < a";

    let norm1 = Canonicalizer::new(&dialect).normalize_sql(sql1).unwrap();
    let norm2 = Canonicalizer::new(&dialect).normalize_sql(sql2).unwrap();

    assert_ne!(norm1, norm2);
}

#[test]
fn test_normalize_error_parse_failure() {
    let dialect = PostgreSqlDialect {};

    let invalid_sql = "NOT VALID SQL ;;;";
    let result = Canonicalizer::new(&dialect).normalize_sql(invalid_sql);

    assert!(matches!(result, Err(CanonicalizeError::Parse { .. })));
}

#[test]
fn test_normalize_error_multiple_statements() {
    let dialect = PostgreSqlDialect {};

    let sql = "SELECT * FROM t WHERE a = 1; SELECT * FROM t WHERE b = 2";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql);

    assert!(matches!(result, Err(CanonicalizeError::Unsupported(_))));
}

#[test]
fn test_normalize_rejects_unbalanced_open_parens() {
    let dialect = PostgreSqlDialect {};
    let err = Canonicalizer::new(&dialect)
        .normalize_sql("SELECT * FROM t WHERE ((((a = 1")
        .unwrap_err();
    assert!(matches!(err, CanonicalizeError::Unsupported(ref m) if m.contains("Unbalanced")));
}

#[test]
fn test_normalize_no_where_clause() {
    let dialect = PostgreSqlDialect {};

    let sql = "SELECT * FROM t";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();

    assert_eq!(result, "true");
}

#[test]
fn test_normalize_all_operators() {
    let dialect = PostgreSqlDialect {};

    for op in &["=", "!=", "<", ">", "<=", ">="] {
        let sql = format!("SELECT * FROM t WHERE a {} b", op);
        let result = Canonicalizer::new(&dialect).normalize_sql(&sql);
        assert!(result.is_ok(), "Failed on operator: {}", op);
    }

    for op in &["AND", "OR"] {
        let sql = format!("SELECT * FROM t WHERE a = 1 {} b = 2", op);
        let result = Canonicalizer::new(&dialect).normalize_sql(&sql);
        assert!(result.is_ok(), "Failed on operator: {}", op);
    }
}

#[test]
fn test_normalize_arithmetic_operators() {
    let dialect = PostgreSqlDialect {};

    for op in &["+", "-", "*", "/", "%"] {
        let sql = format!("SELECT * FROM t WHERE a {} b > 10", op);
        let result = Canonicalizer::new(&dialect).normalize_sql(&sql);
        assert!(result.is_ok(), "Failed on arithmetic operator: {}", op);
    }
}

#[test]
fn test_normalize_not_operator() {
    let dialect = PostgreSqlDialect {};

    let sql1 = "SELECT * FROM t WHERE NOT (a = 1)";
    let sql2 = "SELECT * FROM t WHERE a != 1";

    let norm1 = Canonicalizer::new(&dialect).normalize_sql(sql1).unwrap();
    let norm2 = Canonicalizer::new(&dialect).normalize_sql(sql2).unwrap();

    assert_ne!(norm1, norm2);
}

#[test]
fn test_normalize_complex_nested_expression() {
    let dialect = PostgreSqlDialect {};

    let sql = "SELECT * FROM t WHERE ((a = 1 AND b = 2) OR (c = 3 AND d = 4)) AND e = 5";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql);

    assert!(result.is_ok());
}

#[test]
fn test_normalize_in_list_order() {
    let dialect = PostgreSqlDialect {};

    let sql1 = "SELECT * FROM t WHERE status IN ('active', 'pending', 'processing')";
    let sql2 = "SELECT * FROM t WHERE status IN ('processing', 'active', 'pending')";

    let norm1 = Canonicalizer::new(&dialect).normalize_sql(sql1).unwrap();
    let norm2 = Canonicalizer::new(&dialect).normalize_sql(sql2).unwrap();

    let _ = (norm1, norm2);
}

#[test]
fn test_hash_consistency() {
    let s = "age > 18 AND status = 'active'";

    let hash1 = hash_canonical(s);
    let hash2 = hash_canonical(s);
    let hash3 = hash_canonical(s);

    assert_eq!(hash1, hash2);
    assert_eq!(hash2, hash3);
}

#[test]
fn test_hash_empty_string() {
    let hash = hash_canonical("");
    assert!(hash > 0);
}

#[test]
fn test_hash_long_string() {
    let long_str = "a".repeat(10000);
    let hash = hash_canonical(&long_str);
    assert!(hash > 0);
}

#[test]
fn test_normalize_error_multiple_tables() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t1, t2 WHERE a = 1";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql);
    assert!(matches!(result, Err(CanonicalizeError::Unsupported(_))));
    if let Err(CanonicalizeError::Unsupported(msg)) = result {
        assert!(msg.contains("Exactly one table"));
    }
}

#[test]
fn test_normalize_error_joins() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t1 JOIN t2 ON t1.id = t2.id WHERE a = 1";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql);
    assert!(matches!(result, Err(CanonicalizeError::Unsupported(_))));
    if let Err(CanonicalizeError::Unsupported(msg)) = result {
        assert!(msg.contains("JOINs not supported"));
    }
}

#[test]
fn test_normalize_error_derived_table() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM (SELECT * FROM t1) AS d WHERE d.a = 1";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql);
    assert!(matches!(result, Err(CanonicalizeError::Unsupported(_))));
    if let Err(CanonicalizeError::Unsupported(msg)) = result {
        assert!(msg.contains("Subqueries and derived tables not supported"));
    }
}

#[test]
fn test_normalize_error_non_select_query() {
    let dialect = PostgreSqlDialect {};

    let insert_sql = "INSERT INTO t VALUES (1, 2)";
    let result = Canonicalizer::new(&dialect).normalize_sql(insert_sql);
    assert!(matches!(result, Err(CanonicalizeError::Unsupported(_))));

    let update_sql = "UPDATE t SET a = 1";
    let result = Canonicalizer::new(&dialect).normalize_sql(update_sql);
    assert!(matches!(result, Err(CanonicalizeError::Unsupported(_))));

    let delete_sql = "DELETE FROM t WHERE a = 1";
    let result = Canonicalizer::new(&dialect).normalize_sql(delete_sql);
    assert!(matches!(result, Err(CanonicalizeError::Unsupported(_))));
}

#[test]
fn test_normalize_is_null() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE age IS NULL";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("IS NULL"));
}

#[test]
fn test_normalize_is_not_null() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE age IS NOT NULL";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("IS NOT NULL"));
}

#[test]
fn test_normalize_between() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE age BETWEEN 18 AND 65";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("BETWEEN"));
    assert!(result.contains("18"));
    assert!(result.contains("65"));
}

#[test]
fn test_normalize_not_between() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE age NOT BETWEEN 18 AND 65";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("NOT BETWEEN"));
}

#[test]
fn test_normalize_like() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE name LIKE 'John%'";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("LIKE"));
}

#[test]
fn test_normalize_not_like() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE name NOT LIKE 'John%'";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("NOT LIKE"));
}

#[test]
fn test_normalize_like_with_escape() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE name LIKE 'John\\%' ESCAPE '\\'";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("LIKE"));
    assert!(result.contains("ESCAPE"));
}

#[test]
fn test_normalize_ilike() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE name ILIKE 'john%'";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("ILIKE"));
}

#[test]
fn test_normalize_not_ilike() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE name NOT ILIKE 'john%'";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("NOT ILIKE"));
}

#[test]
fn test_normalize_ilike_with_escape() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE name ILIKE 'john\\%' ESCAPE '\\'";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("ILIKE"));
    assert!(result.contains("ESCAPE"));
}

#[test]
fn test_normalize_compound_identifier() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE schema.table.column = 1";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("schema.table.column"));
}

#[test]
fn test_normalize_unary_plus() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE +age = 10";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("+"));
}

#[test]
fn test_normalize_unary_minus() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE -balance > 100";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("-"));
}

#[test]
fn test_normalize_not_in_list() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE status NOT IN ('active', 'pending')";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    assert!(result.contains("NOT IN"));
}

#[test]
fn global_membership_keeps_its_modifier() {
    assert_canonical(
        &ClickHouseDialect {},
        &[
            ("a GLOBAL IN (3, 1, 2)", "a GLOBAL IN (1, 2, 3)"),
            ("a GLOBAL NOT IN (2, 1)", "a GLOBAL NOT IN (1, 2)"),
            ("a IN (2, 1)", "a IN (1, 2)"),
            ("a NOT IN (2, 1)", "a NOT IN (1, 2)"),
            (
                "a GLOBAL IN (SELECT b FROM u)",
                "a GLOBAL IN (SELECT b FROM u)",
            ),
            (
                "a GLOBAL NOT IN (SELECT b FROM u)",
                "a GLOBAL NOT IN (SELECT b FROM u)",
            ),
            ("a IN (SELECT b FROM u)", "a IN (SELECT b FROM u)"),
        ],
    );
}

#[test]
fn test_error_set_operations() {
    let dialect = PostgreSqlDialect {};

    let sql = "SELECT * FROM t WHERE a = 1 UNION SELECT * FROM t WHERE b = 2";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql);

    assert!(result.is_err());
}

#[test]
fn test_fallback_expression_is_idempotent() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE CAST(a AS text) = 'hello'";
    let normalized = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    let replay = format!("SELECT * FROM t WHERE {normalized}");
    assert_eq!(
        Canonicalizer::new(&dialect).normalize_sql(&replay).unwrap(),
        normalized
    );
}

#[test]
fn test_boolean_truth_test_is_idempotent() {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE enabled IS TRUE";
    let normalized = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();
    let replay = format!("SELECT * FROM t WHERE {normalized}");
    assert_eq!(
        Canonicalizer::new(&dialect).normalize_sql(&replay).unwrap(),
        normalized
    );
}

#[test]
fn test_normalize_unknown_unary_op_fallback() {
    let dialect = PostgreSqlDialect {};

    let sql = "SELECT * FROM t WHERE ~a = 1";
    let result = Canonicalizer::new(&dialect).normalize_sql(sql);

    assert!(matches!(result, Err(CanonicalizeError::Unsupported(_))));
}

#[test]
fn test_and_tree_flattening() {
    let dialect = PostgreSqlDialect {};

    let sql1 = "SELECT * FROM t WHERE a = 1 AND b = 2 AND c = 3";
    let sql2 = "SELECT * FROM t WHERE (a = 1 AND b = 2) AND c = 3";
    let sql3 = "SELECT * FROM t WHERE a = 1 AND (b = 2 AND c = 3)";

    let norm1 = Canonicalizer::new(&dialect).normalize_sql(sql1).unwrap();
    let norm2 = Canonicalizer::new(&dialect).normalize_sql(sql2).unwrap();
    let norm3 = Canonicalizer::new(&dialect).normalize_sql(sql3).unwrap();

    assert_eq!(norm1, norm2, "Flat AND should equal left-associated AND");
    assert_eq!(norm1, norm3, "Flat AND should equal right-associated AND");
}

#[test]
fn test_or_tree_flattening() {
    let dialect = PostgreSqlDialect {};

    let sql1 = "SELECT * FROM t WHERE a = 1 OR b = 2 OR c = 3";
    let sql2 = "SELECT * FROM t WHERE (a = 1 OR b = 2) OR c = 3";
    let sql3 = "SELECT * FROM t WHERE a = 1 OR (b = 2 OR c = 3)";

    let norm1 = Canonicalizer::new(&dialect).normalize_sql(sql1).unwrap();
    let norm2 = Canonicalizer::new(&dialect).normalize_sql(sql2).unwrap();
    let norm3 = Canonicalizer::new(&dialect).normalize_sql(sql3).unwrap();

    assert_eq!(norm1, norm2);
    assert_eq!(norm1, norm3);
}

#[test]
fn test_distinct_operators_produce_different_strings() {
    let dialect = PostgreSqlDialect {};

    let sql1 = "SELECT * FROM t WHERE a + b > 0";
    let sql2 = "SELECT * FROM t WHERE a - b > 0";

    let norm1 = Canonicalizer::new(&dialect).normalize_sql(sql1).unwrap();
    let norm2 = Canonicalizer::new(&dialect).normalize_sql(sql2).unwrap();

    assert_ne!(
        norm1, norm2,
        "'+' and '-' must produce different normalized strings"
    );
}

fn unsupported_message(sql: &str) -> String {
    match Canonicalizer::new(&PostgreSqlDialect {}).normalize_sql(sql) {
        Err(CanonicalizeError::Unsupported(message)) => message,
        other => panic!("expected an unsupported error for {sql}, got {other:?}"),
    }
}

#[test]
fn test_reject_sql_beyond_length_limit() {
    let padding = " ".repeat(8193);
    let sql = format!("SELECT * FROM t WHERE a = 1{padding}");
    assert_eq!(
        Canonicalizer::new(&PostgreSqlDialect {}).normalize_sql(&sql),
        Err(CanonicalizeError::InputTooLong { limit: 8192 })
    );
}

#[test]
fn test_reject_control_character() {
    assert_eq!(
        unsupported_message("SELECT * FROM t WHERE a = \u{1}"),
        "Control character in SQL"
    );
}

#[test]
fn test_reject_unbalanced_square_bracket() {
    assert_eq!(
        unsupported_message("SELECT * FROM t WHERE [a = 1"),
        "Unbalanced square brackets"
    );
}

#[test]
fn test_reject_uncanonicalizable_binary_operator() {
    assert_eq!(
        unsupported_message("SELECT * FROM t WHERE a # b = 1"),
        "Unsupported binary operator: #"
    );
}

#[test]
fn test_reject_literal_that_loses_a_quote_level() {
    assert!(matches!(
        Canonicalizer::new(&PostgreSqlDialect {}).normalize_sql("SELECT * FROM t WHERE a = ''''''"),
        Err(CanonicalizeError::NotRoundTrippable(_))
    ));
}

/// MySQL reads a backslash as escaping the closing quote, so a `LIKE` pattern whose value
/// ends in one has no canonical spelling MySQL accepts. The payload is the rendered text, so
/// the refusal is provably the confirmation read, and PostgreSQL accepting the same bytes
/// shows it is the dialect's verdict. subql relies on this to drop such subscriptions.
#[test]
fn test_reject_mysql_pattern_ending_with_the_escape() {
    let dialect = MySqlDialect {};
    let sql = "SELECT * FROM t WHERE name LIKE 'a\\\\'";
    let canonicalizer = Canonicalizer::new(&dialect);
    let statement_verdict = canonicalizer.normalize_sql(sql);
    assert!(matches!(
        &statement_verdict,
        Err(CanonicalizeError::NotRoundTrippable(text)) if text == "name LIKE 'a\\'"
    ));

    let mut statements = Parser::parse_sql(&dialect, sql).unwrap();
    let Statement::Query(query) = statements.pop().unwrap() else {
        panic!("the test SQL is a query");
    };
    let SetExpr::Select(select) = *query.body else {
        panic!("the test SQL is a plain SELECT");
    };
    let clause_verdict = canonicalizer.normalize_where_clause(select.selection.as_ref());
    assert!(matches!(
        &clause_verdict,
        Err(CanonicalizeError::NotRoundTrippable(text)) if text == "name LIKE 'a\\'"
    ));

    let postgres = Canonicalizer::new(&PostgreSqlDialect {});
    assert_eq!(
        postgres
            .normalize_sql("SELECT * FROM t WHERE name LIKE 'a\\'")
            .unwrap(),
        "name LIKE 'a\\'",
    );
}

/// A chain's canonical text is one level deep however many terms it has, so reading it back
/// stays within the parser's recursion limit.
#[test]
fn test_long_boolean_chain_is_spelled_flat() {
    let canonicalizer = Canonicalizer::new(&PostgreSqlDialect {});
    for operator in ["AND", "OR"] {
        let terms: Vec<String> = (0..300).map(|term| format!("c{term} = {term}")).collect();
        let sql = format!(
            "SELECT * FROM t WHERE {}",
            terms.join(&format!(" {operator} "))
        );
        let mut expected: Vec<String> =
            (0..300).map(|term| format!("({term} = c{term})")).collect();
        expected.sort();
        assert_eq!(
            canonicalizer.normalize_sql(&sql).unwrap(),
            format!("({})", expected.join(&format!(" {operator} ")))
        );
    }
}

/// A delimited name escapes its own delimiter by doubling it, and the canonical spelling has
/// to do the same or it names something else. subql keys rows on such a column.
#[test]
fn test_quoted_identifier_carrying_its_delimiter_is_escaped() {
    let canonicalizer = Canonicalizer::new(&PostgreSqlDialect {});
    let canonical = canonicalizer
        .normalize_sql("SELECT * FROM t WHERE \"a\"\"b\" = 7")
        .unwrap();
    assert_eq!(canonical, "(\"a\"\"b\" = 7)");

    // Two doubled delimiters in the name survive the same way.
    let deeper = canonicalizer
        .normalize_sql("SELECT * FROM t WHERE \"a\"\"\"\"b\" = 1")
        .unwrap();
    assert_eq!(deeper, "(\"a\"\"\"\"b\" = 1)");
}

fn assert_canonical(dialect: &dyn Dialect, cases: &[(&str, &str)]) {
    let canonicalizer = Canonicalizer::new(dialect);
    for (predicate, expected) in cases {
        let sql = format!("SELECT * FROM t WHERE {predicate}");
        assert_eq!(
            canonicalizer.normalize_sql(&sql).as_deref(),
            Ok(*expected),
            "{dialect:?} {predicate}"
        );
    }
}

#[test]
fn predicate_operands_keep_their_parentheses() {
    let cases = [
        ("flag = (value IS NULL)", "((value IS NULL) = flag)"),
        ("(a IS NULL) = b", "((a IS NULL) = b)"),
        ("flag = (v IS NOT NULL)", "((v IS NOT NULL) = flag)"),
        ("flag = (v IN (1, 2))", "((v IN (1, 2)) = flag)"),
        (
            "flag < (v IN (SELECT id FROM u))",
            "((v IN (SELECT id FROM u)) > flag)",
        ),
        (
            "flag = (v NOT BETWEEN 1 AND 2)",
            "((v NOT BETWEEN 1 AND 2) = flag)",
        ),
        ("flag = (v LIKE 'x')", "((v LIKE 'x') = flag)"),
        ("(a IS NULL) IS NULL", "(a IS NULL) IS NULL"),
        ("v BETWEEN (a IS NULL) AND b", "v BETWEEN (a IS NULL) AND b"),
        ("NOT (a IS NULL)", "NOT (a IS NULL)"),
    ];
    assert_canonical(&PostgreSqlDialect {}, &cases);
    assert_canonical(&MySqlDialect {}, &cases);
    assert_canonical(
        &PostgreSqlDialect {},
        &[("flag = (v ILIKE 'x')", "((v ILIKE 'x') = flag)")],
    );
}

#[test]
fn predicates_that_are_not_operands_stay_unparenthesized() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("(a IS NULL)", "a IS NULL"),
            ("(v LIKE 'x') AND (w IN (1))", "(v LIKE 'x' AND w IN (1))"),
        ],
    );
}

fn canonical_texts(dialect: &dyn Dialect, predicates: &[&str]) -> Vec<String> {
    let canonicalizer = Canonicalizer::new(dialect);
    predicates
        .iter()
        .map(|predicate| {
            canonicalizer
                .normalize_sql(&format!("SELECT * FROM t WHERE {predicate}"))
                .unwrap()
        })
        .collect()
}

#[test]
fn null_safe_equality_orders_its_operands() {
    let texts = canonical_texts(&MySqlDialect {}, &["n <=> m", "M <=> N", "m <=> (n)"]);
    assert_eq!(texts, ["(m <=> n)"; 3]);
}

#[test]
fn is_not_distinct_from_normalizes_and_orders_its_operands() {
    let texts = canonical_texts(
        &PostgreSqlDialect {},
        &[
            "n IS NOT DISTINCT FROM m",
            "M IS NOT DISTINCT FROM N",
            "m IS NOT DISTINCT FROM (n)",
        ],
    );
    assert_eq!(texts, ["(m IS NOT DISTINCT FROM n)"; 3]);
}

#[test]
fn is_distinct_from_normalizes_and_orders_its_operands() {
    let texts = canonical_texts(
        &PostgreSqlDialect {},
        &["n IS DISTINCT FROM m", "M IS DISTINCT FROM (N)"],
    );
    assert_eq!(texts, ["(m IS DISTINCT FROM n)"; 2]);
}

#[test]
fn inequality_orders_its_operands() {
    let texts = canonical_texts(&PostgreSqlDialect {}, &["a != b", "b <> a"]);
    assert_eq!(texts, ["(a != b)"; 2]);
}

#[test]
fn nested_distinctness_tests_read_back_as_themselves() {
    let texts = canonical_texts(
        &PostgreSqlDialect {},
        &[
            "(a IS DISTINCT FROM b) = c",
            "c IS NOT DISTINCT FROM (b IS DISTINCT FROM a)",
        ],
    );
    assert_eq!(
        texts,
        [
            "((a IS DISTINCT FROM b) = c)",
            "((a IS DISTINCT FROM b) IS NOT DISTINCT FROM c)",
        ]
    );
}

#[test]
fn null_safe_comparisons_keep_predicate_operands_enclosed() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            (
                "flag IS DISTINCT FROM (value IS NULL)",
                "((value IS NULL) IS DISTINCT FROM flag)",
            ),
            (
                "(value IS NULL) IS NOT DISTINCT FROM flag",
                "((value IS NULL) IS NOT DISTINCT FROM flag)",
            ),
            (
                "z IS DISTINCT FROM (v LIKE 'x')",
                "((v LIKE 'x') IS DISTINCT FROM z)",
            ),
            (
                "a IS NOT DISTINCT FROM (v IN (1, 2))",
                "((v IN (1, 2)) IS NOT DISTINCT FROM a)",
            ),
        ],
    );
    assert_canonical(
        &MySqlDialect {},
        &[
            ("flag <=> (value IS NULL)", "((value IS NULL) <=> flag)"),
            ("(b IS NULL) != a", "((b IS NULL) != a)"),
            ("z != (b IS NULL)", "((b IS NULL) != z)"),
        ],
    );
}

#[test]
fn a_predicate_as_operand_keeps_its_grouping() {
    let postgres = [
        ("z LIKE (x IS TRUE)", "z LIKE (x IS TRUE)"),
        ("z LIKE x IS TRUE", "(z LIKE x) IS TRUE"),
        ("z NOT ILIKE (x IS UNKNOWN)", "z NOT ILIKE (x IS UNKNOWN)"),
        ("z NOT ILIKE x IS UNKNOWN", "(z NOT ILIKE x) IS UNKNOWN"),
        ("z BETWEEN 1 AND (x IS TRUE)", "z BETWEEN 1 AND (x IS TRUE)"),
        ("z BETWEEN 1 AND x IS TRUE", "(z BETWEEN 1 AND x) IS TRUE"),
        ("z LIKE ((x IS TRUE))", "z LIKE (x IS TRUE)"),
        ("z LIKE (x = ANY(y))", "z LIKE (x = ANY(y))"),
        ("z LIKE (x SIMILAR TO 'y')", "z LIKE (x SIMILAR TO 'y')"),
    ];
    assert_canonical(&PostgreSqlDialect {}, &postgres);
    let mysql = [
        ("(x REGEXP 'y') IN (1)", "(x RLIKE 'y') IN (1)"),
        ("x REGEXP 'y' IN (1)", "x RLIKE ('y' IN (1))"),
        (
            "(x RLIKE 'y') BETWEEN 1 AND 2",
            "(x RLIKE 'y') BETWEEN 1 AND 2",
        ),
        (
            "x RLIKE 'y' BETWEEN 1 AND 2",
            "x RLIKE ('y' BETWEEN 1 AND 2)",
        ),
        ("z LIKE (x MEMBER OF (y))", "z LIKE (x MEMBER OF(y))"),
    ];
    assert_canonical(&MySqlDialect {}, &mysql);
}

#[test]
fn a_predicate_as_operand_is_accepted() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("- (x IS TRUE)", "- (x IS TRUE)"),
            ("z BETWEEN (x IS TRUE) AND 2", "z BETWEEN (x IS TRUE) AND 2"),
            ("(x IS NOT FALSE) != z", "((x IS NOT FALSE) != z)"),
        ],
    );
    assert_canonical(
        &MySqlDialect {},
        &[("(x SIMILAR TO 'y') = z", "((x SIMILAR TO 'y') = z)")],
    );
}

#[test]
fn mysql_keeps_the_case_of_a_table_qualifier() {
    assert_canonical(
        &MySqlDialect {},
        &[
            ("T.a = 1", "(1 = T.a)"),
            ("t.A = 1", "(1 = t.a)"),
            ("`T`.a = 1", "(1 = `T`.a)"),
        ],
    );
    assert_canonical(&PostgreSqlDialect {}, &[("T.A = 1", "(1 = t.a)")]);
}

#[test]
fn truth_tests_normalize_their_operand() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("A IS TRUE", "a IS TRUE"),
            ("(a) IS NOT TRUE", "a IS NOT TRUE"),
            ("A IS FALSE", "a IS FALSE"),
            ("((a)) IS NOT FALSE", "a IS NOT FALSE"),
            ("A IS UNKNOWN", "a IS UNKNOWN"),
            ("A IS NOT UNKNOWN", "a IS NOT UNKNOWN"),
            ("(A = 1) IS TRUE", "(1 = a) IS TRUE"),
        ],
    );
}

#[test]
fn pattern_and_regular_expression_matches_normalize_their_operands() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("(X) SIMILAR TO 'y%'", "x SIMILAR TO 'y%'"),
            (
                "X NOT SIMILAR TO 'a!%' ESCAPE '!'",
                "x NOT SIMILAR TO 'a!%' ESCAPE '!'",
            ),
        ],
    );
    assert_canonical(
        &MySqlDialect {},
        &[
            ("X REGEXP 'y'", "x RLIKE 'y'"),
            ("x RLIKE 'y'", "x RLIKE 'y'"),
            ("X NOT REGEXP ('y')", "x NOT RLIKE 'y'"),
        ],
    );
    assert_canonical(&SQLiteDialect {}, &[("X RLIKE 'y'", "x RLIKE 'y'")]);
}

#[test]
fn quantified_comparisons_normalize_their_operands() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("X = SOME(Arr)", "x = ANY(arr)"),
            ("x = ANY((arr))", "x = ANY(arr)"),
            ("X <> ALL(Arr)", "x != ALL(arr)"),
        ],
    );
}

#[test]
fn json_and_normalization_tests_normalize_their_operand() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("D IS JSON", "d IS JSON"),
            ("(d) IS NOT JSON OBJECT", "d IS NOT JSON OBJECT"),
            ("S IS NFC NORMALIZED", "s IS NFC NORMALIZED"),
            ("(s) IS NOT NORMALIZED", "s IS NOT NORMALIZED"),
        ],
    );
}

#[test]
fn time_zone_and_collation_normalize_their_operand() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            (
                "(T) AT TIME ZONE 'UTC' > now()",
                "((t AT TIME ZONE 'UTC') > now())",
            ),
            ("Name COLLATE \"C\" = 'x'", "('x' = (name COLLATE \"C\"))"),
        ],
    );
}

#[test]
fn mysql_membership_and_full_text_search_normalize_their_names() {
    assert_canonical(
        &MySqlDialect {},
        &[
            ("3 MEMBER OF(J)", "3 MEMBER OF(j)"),
            (
                "MATCH (Title, Body) AGAINST ('x' IN BOOLEAN MODE)",
                "MATCH (title, body) AGAINST ('x' IN BOOLEAN MODE)",
            ),
        ],
    );
}

#[test]
fn a_quantified_pattern_match_keeps_its_quantifier() {
    for dialect in [&PostgreSqlDialect {} as &dyn Dialect, &MySqlDialect {}] {
        assert_canonical(
            dialect,
            &[
                ("x LIKE ANY ('a', 'b')", "x LIKE ANY ('a', 'b')"),
                ("x LIKE ('a', 'b')", "x LIKE ('a', 'b')"),
                ("X NOT ILIKE ANY (('a', 'b'))", "x NOT ILIKE ANY ('a', 'b')"),
            ],
        );
    }
}

#[test]
fn a_call_to_a_function_named_like_a_quantifier_is_refused() {
    for predicate in ["z LIKE (ANY(x) = 1)", "SOME(x) != 1", "ALL(x) < 1"] {
        assert!(
            matches!(
                Canonicalizer::new(&PostgreSqlDialect {})
                    .normalize_sql(&format!("SELECT * FROM t WHERE {predicate}")),
                Err(CanonicalizeError::Unsupported(_))
            ),
            "{predicate}"
        );
    }
    assert_canonical(
        &PostgreSqlDialect {},
        &[("z LIKE (1 = ANY(x))", "z LIKE (1 = ANY(x))")],
    );
}

#[test]
fn json_path_access_is_refused() {
    // sqlparser reads `:` as a JSON path in dialects without one, and can build a tree its
    // printer spells as a different grouping.
    for predicate in [
        "a:b = 1",
        "x = b:c && d",
        "$$=I:OPz-T:t&&MMEkzte:I:$$=IME&&ME",
    ] {
        assert!(
            matches!(
                Canonicalizer::new(&MySqlDialect {})
                    .normalize_sql(&format!("SELECT * FROM t WHERE {predicate}")),
                Err(CanonicalizeError::Unsupported(_))
            ),
            "{predicate}"
        );
    }
}

fn assert_unsupported(dialect: &dyn Dialect, predicates: &[&str]) {
    let canonicalizer = Canonicalizer::new(dialect);
    for predicate in predicates {
        let sql = format!("SELECT * FROM t WHERE {predicate}");
        assert!(
            matches!(
                canonicalizer.normalize_sql(&sql),
                Err(CanonicalizeError::Unsupported(_))
            ),
            "{dialect:?} {predicate}"
        );
    }
}

#[test]
fn function_calls_fold_their_name_and_normalize_their_arguments() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("COALESCE(A, 1) > 0", "(0 < coalesce(a, 1))"),
            ("coalesce((a), 1) > 0", "(0 < coalesce(a, 1))"),
            ("LOWER(Name) = 'x'", "('x' = lower(name))"),
            ("\"Lower\"(name) = 'x'", "(\"Lower\"(name) = 'x')"),
            (
                "Pg_Catalog.Lower(name) = 'x'",
                "('x' = pg_catalog.lower(name))",
            ),
            ("NOW() > d", "(d < now())"),
            ("d < CURRENT_TIMESTAMP", "(current_timestamp > d)"),
            ("f(A = 1, (b))", "f((1 = a), b)"),
        ],
    );
    assert_canonical(
        &MySqlDialect {},
        &[("IFNULL(A, 1) = 2", "(2 = ifnull(a, 1))")],
    );
}

#[test]
fn aggregate_and_window_calls_are_refused() {
    assert_unsupported(
        &PostgreSqlDialect {},
        &[
            "count(*) > 1",
            "max(DISTINCT x) > 1",
            "sum(x) FILTER (WHERE y) > 1",
            "rank() OVER () = 1",
            "string_agg(x, ',' ORDER BY x) = 'a'",
            "f(name => x) = 1",
        ],
    );
}

#[test]
fn casts_share_one_spelling() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("CAST(A AS INT) = 1", "(1 = CAST(a AS INT))"),
            ("A::INT = 1", "(1 = CAST(a AS INT))"),
            ("(a)::INT = 1", "(1 = CAST(a AS INT))"),
            ("cast((a + 1) as int) = 1", "(1 = CAST((1 + a) AS INT))"),
        ],
    );
    assert_canonical(
        &GenericDialect {},
        &[("TRY_CAST(A AS INT) = 1", "(1 = TRY_CAST(A AS INT))")],
    );
}

#[test]
fn conditional_expressions_normalize_every_branch() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            (
                "CASE WHEN A THEN 1 ELSE NULL END = 1",
                "(1 = CASE WHEN a THEN 1 END)",
            ),
            (
                "CASE WHEN (a) THEN 1 END = 1",
                "(1 = CASE WHEN a THEN 1 END)",
            ),
            (
                "CASE A WHEN 1 THEN 'x' ELSE (B) END = 'x'",
                "('x' = CASE a WHEN 1 THEN 'x' ELSE b END)",
            ),
            (
                "CASE WHEN a = 1 THEN B WHEN C IS NULL THEN 2 END = 3",
                "(3 = CASE WHEN (1 = a) THEN b WHEN c IS NULL THEN 2 END)",
            ),
        ],
    );
}

#[test]
fn special_syntax_functions_normalize_their_operands() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            (
                "EXTRACT(YEAR FROM D) = 2020",
                "(2020 = EXTRACT(YEAR FROM d))",
            ),
            ("POSITION('a' IN (S)) = 1", "(1 = POSITION('a' IN s))"),
            (
                "SUBSTRING(S FROM 1 FOR 2) = 'ab'",
                "('ab' = SUBSTRING(s FROM 1 FOR 2))",
            ),
            ("TRIM(BOTH 'x' FROM S) = ''", "('' = TRIM(BOTH 'x' FROM s))"),
            ("CEIL(X) = FLOOR(Y)", "(CEIL(x) = FLOOR(y))"),
            (
                "OVERLAY(S PLACING 'a' FROM 1) = 'b'",
                "('b' = OVERLAY(s PLACING 'a' FROM 1))",
            ),
        ],
    );
    assert_canonical(
        &MySqlDialect {},
        &[
            ("CONVERT(A, CHAR) = 'x'", "('x' = CONVERT(a, CHAR))"),
            (
                "CONVERT(A USING utf8mb4) = 'x'",
                "('x' = CONVERT(a USING utf8mb4))",
            ),
            ("SUBSTR(S, 1, 2) = 'ab'", "('ab' = SUBSTR(s, 1, 2))"),
        ],
    );
}

#[test]
fn tuples_arrays_and_typed_literals_normalize_their_elements() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("(A, (b)) = (1, 2)", "((1, 2) = (a, b))"),
            ("ARRAY[A, 1] = x", "(ARRAY[a, 1] = x)"),
            ("D > DATE '2020-01-01'", "(DATE '2020-01-01' < d)"),
            (
                "D < NOW() - INTERVAL '1' DAY",
                "((now() - (INTERVAL '1' DAY)) > d)",
            ),
        ],
    );
}

#[test]
fn field_access_keeps_the_parentheses_that_decide_its_meaning() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("(C).Field = 1", "((c).field = 1)"),
            ("((c)).field = 1", "((c).field = 1)"),
            ("C.Field[1] = 1", "(1 = c.field[1])"),
            ("A[(1)] = 1", "(1 = a[1])"),
        ],
    );
    // MySQL matches table names by case, so a name that may be a qualifier keeps its spelling.
    assert_canonical(&MySqlDialect {}, &[("T.a[1] = 1", "(1 = T.a[1])")]);
}

#[test]
fn a_call_keeps_its_arguments_apart_from_special_syntax() {
    assert_canonical(
        &AnsiDialect {},
        &[(
            "POSITION(TRUE >= a IN ((2), 2)) = 1",
            "(1 = POSITION(((A <= true) IN (2, 2))))",
        )],
    );
    assert_canonical(
        &PostgreSqlDialect {},
        &[("POSITION('a' IN s) = 1", "(1 = POSITION('a' IN s))")],
    );
}

#[test]
fn a_name_spelled_like_an_operator_is_quoted_or_refused() {
    // Inside `POSITION`, sqlparser reads `NOT` as a column name, and it would read back as the
    // operator anywhere else.
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            (
                "POSITION(NOT - (b) IN a) = 1",
                "(1 = POSITION((\"not\" - b) IN a))",
            ),
            ("Status = 'paid'", "('paid' = status)"),
        ],
    );
    assert_canonical(
        &AnsiDialect {},
        &[(
            "POSITION(NOT - (b) IN a) = 1",
            "(1 = POSITION((\"NOT\" - B) IN A))",
        )],
    );
    assert_canonical(
        &GenericDialect {},
        &[(
            "POSITION(NOT - (b) IN a) = 1",
            "(1 = POSITION((NOT (- b IN a))))",
        )],
    );
}

#[test]
fn a_quoted_function_name_keeps_its_quotes() {
    // MySQL looks a quoted function name up among stored functions only.
    assert_canonical(
        &MySqlDialect {},
        &[
            ("`myfn`(Name) = 'x'", "('x' = `myfn`(name))"),
            ("MyFn(Name) = 'x'", "('x' = myfn(name))"),
        ],
    );
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("\"myfn\"(name) = 'x'", "(\"myfn\"(name) = 'x')"),
            ("MyFn(name) = 'x'", "('x' = myfn(name))"),
        ],
    );
}

#[test]
fn dialect_specific_value_forms_normalize_their_operands() {
    assert_canonical(
        &BigQueryDialect {},
        &[
            ("SAFE_CAST(A AS INT64) = 1", "(1 = SAFE_CAST(A AS INT64))"),
            ("TRIM(S, 'x') = 'y'", "('y' = TRIM(S, 'x'))"),
        ],
    );
    assert_canonical(
        &SnowflakeDialect {},
        &[("EXTRACT(YEAR, (D)) = 2020", "(2020 = EXTRACT(YEAR, D))")],
    );
    assert_canonical(
        &MySqlDialect {},
        &[
            (
                "CONVERT(A, CHAR CHARACTER SET utf8mb4) = 'x'",
                "('x' = CONVERT(a, CHAR CHARACTER SET utf8mb4))",
            ),
            ("_utf8mb4'x' = A", "(_utf8mb4 'x' = a)"),
        ],
    );
    assert_canonical(
        &GenericDialect {},
        &[
            ("CEIL(D TO DAY) = D", "(CEIL(D TO DAY) = D)"),
            ("CEIL(X, 2) = FLOOR(Y, 2)", "(CEIL(X, 2) = FLOOR(Y, 2))"),
            ("F((X)).Y = 1", "(1 = F(X).Y)"),
        ],
    );
    assert_canonical(&DuckDbDialect {}, &[("A[1:2:(3)] = B", "(A[1:2:3] = B)")]);
}

#[test]
fn postgres_value_forms_keep_their_qualifiers() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            ("TRIM(LEADING S) = 'x'", "('x' = TRIM(LEADING s))"),
            (
                "OVERLAY(S PLACING 'a' FROM 1 FOR (2)) = 'b'",
                "('b' = OVERLAY(s PLACING 'a' FROM 1 FOR 2))",
            ),
            (
                "D < INTERVAL '1' DAY TO HOUR",
                "((INTERVAL '1' DAY TO HOUR) > d)",
            ),
            ("D < INTERVAL '1' DAY (2)", "((INTERVAL '1' DAY (2)) > d)"),
            (
                "D < INTERVAL '1' SECOND (2, 3)",
                "((INTERVAL '1' SECOND (2, 3)) > d)",
            ),
            (
                "D < INTERVAL '1' HOUR TO SECOND (3)",
                "((INTERVAL '1' HOUR TO SECOND (3)) > d)",
            ),
            ("A[1:2] = B", "(a[1:2] = b)"),
            ("A[:(2)] = B", "(a[:2] = b)"),
            ("(C).F(x) = 1", "((c).f(x) = 1)"),
            ("D IS JSON WITH UNIQUE KEYS", "d IS JSON WITH UNIQUE KEYS"),
        ],
    );
}

#[test]
fn value_forms_without_a_single_spelling_are_refused() {
    assert_unsupported(
        &BigQueryDialect {},
        &["CAST(A AS STRING FORMAT 'YYYY') = 'x'"],
    );
    assert_unsupported(&MsSqlDialect {}, &["CONVERT(INT, A) = 1"]);
    assert_unsupported(
        &GenericDialect {},
        &["{d '2020-01-01'} = D", "x = ARRAY(SELECT 1)"],
    );
}

#[test]
fn subqueries_normalize_their_projection_table_and_filter() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            (
                "x IN (SELECT Id FROM M WHERE Owner = 'a')",
                "x IN (SELECT id FROM m WHERE ('a' = owner))",
            ),
            (
                "EXISTS (SELECT * FROM U WHERE U.a = T.b)",
                "EXISTS (SELECT * FROM u WHERE (t.b = u.a))",
            ),
            (
                "NOT EXISTS (SELECT 1 FROM U)",
                "NOT EXISTS (SELECT 1 FROM u)",
            ),
            ("x = (SELECT max(Y) FROM u)", "((SELECT max(y) FROM u) = x)"),
            ("x = ANY (SELECT Y FROM u)", "x = ANY(SELECT y FROM u)"),
            ("x = ANY ((SELECT Y FROM u))", "x = ANY(SELECT y FROM u)"),
        ],
    );
    // MySQL matches table names by case, so a subquery's table keeps its spelling.
    assert_canonical(
        &MySqlDialect {},
        &[
            ("x IN (SELECT ID FROM M)", "x IN (SELECT id FROM M)"),
            ("x IN (SELECT id FROM m)", "x IN (SELECT id FROM m)"),
        ],
    );
}

#[test]
fn a_subquery_table_alias_is_kept_folded_as_a_qualifier() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            (
                "EXISTS (SELECT 1 FROM shares s WHERE s.doc_id = docs.id)",
                "EXISTS (SELECT 1 FROM shares s WHERE (docs.id = s.doc_id))",
            ),
            (
                "EXISTS (SELECT 1 FROM Shares AS S WHERE S.doc_id = docs.id)",
                "EXISTS (SELECT 1 FROM shares s WHERE (docs.id = s.doc_id))",
            ),
            (
                "x IN (SELECT a FROM u AS \"v\" WHERE \"v\".a = 1)",
                "x IN (SELECT a FROM u v WHERE (1 = v.a))",
            ),
            (
                "x IN (SELECT a FROM u \"V\")",
                "x IN (SELECT a FROM u \"V\")",
            ),
            (
                "x IN (SELECT a FROM u AS \"where\")",
                "x IN (SELECT a FROM u \"where\")",
            ),
        ],
    );
    // MySQL matches table names by case, and a table alias with them.
    assert_canonical(
        &MySqlDialect {},
        &[
            ("x IN (SELECT a FROM m AS V)", "x IN (SELECT a FROM m V)"),
            ("x IN (SELECT a FROM m v)", "x IN (SELECT a FROM m v)"),
        ],
    );
}

#[test]
fn a_subquery_table_keeps_only_and_drops_the_descendants_star() {
    for dialect in [&PostgreSqlDialect {} as &dyn Dialect, &GenericDialect {}] {
        assert_canonical(
            dialect,
            &[
                ("x IN (SELECT a FROM ONLY u)", "x IN (SELECT a FROM ONLY u)"),
                (
                    "x IN (SELECT a FROM ONLY (u) AS v WHERE v.a = 1)",
                    "x IN (SELECT a FROM ONLY u v WHERE (1 = v.a))",
                ),
                ("x IN (SELECT a FROM u *)", "x IN (SELECT a FROM u)"),
                (
                    "EXISTS (SELECT 1 FROM s.u * v WHERE v.a = 1)",
                    "EXISTS (SELECT 1 FROM s.u v WHERE (1 = v.a))",
                ),
            ],
        );
    }
}

#[test]
fn a_list_never_opens_with_an_item_read_as_a_query() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            (
                "amount IN (1, ((SELECT amount FROM orders)))",
                "amount IN (1, (SELECT amount FROM orders))",
            ),
            (
                "x IN (2, (SELECT b FROM o), (SELECT a FROM o))",
                "x IN (2, (SELECT a FROM o), (SELECT b FROM o))",
            ),
            (
                "x IN (2, (SELECT a FROM o)[1])",
                "x IN (((SELECT a FROM o)[1]), 2)",
            ),
            (
                "x IN (((SELECT a FROM o)[1]), (SELECT b FROM o))",
                "x IN (((SELECT a FROM o)[1]), (SELECT b FROM o))",
            ),
            (
                "x NOT IN (3, 2, (SELECT a FROM o))",
                "x NOT IN (2, (SELECT a FROM o), 3)",
            ),
            // A list whose sorted head already reads as an item keeps its order and spelling.
            (
                "x IN (1, (SELECT b FROM o), 'a')",
                "x IN ('a', (SELECT b FROM o), 1)",
            ),
            (
                "x IN (((SELECT a FROM o)[1]), 'a')",
                "x IN ('a', (SELECT a FROM o)[1])",
            ),
        ],
    );
}

#[test]
fn subqueries_outside_the_served_shape_are_refused() {
    assert_unsupported(
        &PostgreSqlDialect {},
        &[
            "x IN (SELECT a FROM u GROUP BY a)",
            "x IN (SELECT a FROM u GROUP BY a HAVING count(a) > 1)",
            "x IN (SELECT a FROM u ORDER BY a)",
            "x IN (SELECT a FROM u LIMIT 1)",
            "x IN (SELECT DISTINCT a FROM u)",
            "x IN (SELECT a AS b FROM u)",
            "x IN (SELECT a FROM u AS v (b))",
            "x IN (SELECT a FROM generate_series(1, 3))",
            "x IN (SELECT a FROM u TABLESAMPLE BERNOULLI (10))",
            "x IN (SELECT a FROM u JOIN v ON u.a = v.a)",
            "x IN (SELECT a FROM u UNION SELECT a FROM v)",
            "EXISTS (SELECT u.* FROM u)",
            "x IN (SELECT a FROM u HAVING count(a) > 1)",
        ],
    );
    assert_unsupported(&MySqlDialect {}, &["x IN (SELECT a FROM u USE INDEX (i))"]);
    assert_unsupported(
        &SnowflakeDialect {},
        &["x IN (SELECT a FROM IDENTIFIER('u'))"],
    );
}

#[test]
fn forms_no_where_clause_serves_are_refused() {
    assert_unsupported(&DuckDbDialect {}, &["{'a': 1} = x", "MAP {'a': 1} = x"]);
    assert_unsupported(
        &BigQueryDialect {},
        &["STRUCT(1 AS a) = x", "x IN UNNEST(arr)"],
    );
    assert_unsupported(&DatabricksDialect {}, &["transform(a, x -> x + 1) = b"]);
    assert_unsupported(&SnowflakeDialect {}, &["a(+) = b"]);
}

#[test]
fn a_subquery_filtered_by_true_is_the_unfiltered_subquery() {
    assert_canonical(
        &PostgreSqlDialect {},
        &[
            (
                "x IN (SELECT Id FROM u WHERE TRUE)",
                "x IN (SELECT id FROM u)",
            ),
            (
                "x IN (SELECT id FROM u WHERE (TRUE))",
                "x IN (SELECT id FROM u)",
            ),
            ("x IN (SELECT id FROM u)", "x IN (SELECT id FROM u)"),
            (
                "x IN (SELECT id FROM u WHERE FALSE)",
                "x IN (SELECT id FROM u WHERE false)",
            ),
        ],
    );
}

#[test]
fn a_caller_built_form_no_predicate_holds_is_refused() {
    let name = || Box::new(Expr::Identifier(Ident::new("a")));
    for expr in [
        Expr::Wildcard(AttachedToken::empty()),
        Expr::Prior(name()),
        Expr::GroupingSets(vec![vec![*name()]]),
        Expr::Named {
            expr: name(),
            name: Ident::new("b"),
        },
    ] {
        assert!(
            matches!(
                Canonicalizer::new(&PostgreSqlDialect {}).normalize_where_clause(Some(&expr)),
                Err(CanonicalizeError::Unsupported(_))
            ),
            "{expr:?}"
        );
    }
}

#[test]
fn a_missing_filter_is_the_filter_true_where_true_is_reserved() {
    for dialect in [
        &PostgreSqlDialect {} as &dyn Dialect,
        &MySqlDialect {},
        &AnsiDialect {},
    ] {
        let canonicalizer = Canonicalizer::new(dialect);
        for sql in [
            "SELECT * FROM t",
            "SELECT * FROM t WHERE TRUE",
            "SELECT * FROM t WHERE (true)",
        ] {
            assert_eq!(
                canonicalizer.normalize_sql(sql).as_deref(),
                Ok("true"),
                "{dialect:?} {sql}"
            );
        }
        assert_eq!(
            canonicalizer.normalize_where_clause(None).as_deref(),
            Ok("true")
        );
    }
}

#[test]
fn a_missing_filter_is_one_equals_one_where_true_may_name_a_column() {
    // SQLite reads a bare `TRUE` as a column named `true` when the table has one.
    for dialect in [&SQLiteDialect {} as &dyn Dialect, &GenericDialect {}] {
        let canonicalizer = Canonicalizer::new(dialect);
        for sql in ["SELECT * FROM t", "SELECT * FROM t WHERE 1 = 1"] {
            assert_eq!(
                canonicalizer.normalize_sql(sql).as_deref(),
                Ok("(1 = 1)"),
                "{dialect:?} {sql}"
            );
        }
        assert_eq!(
            canonicalizer
                .normalize_sql("SELECT * FROM t WHERE TRUE")
                .as_deref(),
            Ok("true")
        );
        assert_eq!(
            canonicalizer
                .normalize_sql("SELECT * FROM t WHERE x IN (SELECT id FROM u WHERE TRUE)")
                .as_deref(),
            Ok("x IN (SELECT id FROM u WHERE true)")
        );
    }
}

#[test]
fn mirrored_comparisons_share_a_key() {
    let cases = [
        ("age > 18", "(18 < age)"),
        ("18 < age", "(18 < age)"),
        ("age >= 18", "(18 <= age)"),
        ("18 <= age", "(18 <= age)"),
        ("a < b", "(a < b)"),
        ("b > a", "(a < b)"),
        ("B >= A", "(a <= b)"),
        ("b > b", "(b < b)"),
        ("b >= (b)", "(b <= b)"),
    ];
    assert_canonical(&PostgreSqlDialect {}, &cases);
}

#[test]
fn comparisons_between_two_columns_keep_their_order_where_the_left_collation_wins() {
    // SQLite compares two columns under the left one's collation, so `b > a` and `a < b` can
    // differ. Against a literal the column's collation applies on either side.
    for dialect in [&SQLiteDialect {} as &dyn Dialect, &GenericDialect {}] {
        assert_canonical(
            dialect,
            &[
                ("b > a", "(b > a)"),
                ("a < b", "(a < b)"),
                ("age > 18", "(18 < age)"),
                ("18 < age", "(18 < age)"),
                ("b > b", "(b < b)"),
            ],
        );
    }
}

#[test]
fn sums_and_products_order_their_operands_where_plus_is_numeric() {
    let cases = [
        ("b + a = 1", "((a + b) = 1)"),
        ("a + b = 1", "((a + b) = 1)"),
        ("b * a = 1", "((a * b) = 1)"),
        ("b - a = 1", "((b - a) = 1)"),
        ("(c + b) + a = 1", "(((b + c) + a) = 1)"),
    ];
    for dialect in [
        &PostgreSqlDialect {} as &dyn Dialect,
        &MySqlDialect {},
        &SQLiteDialect {},
    ] {
        assert_canonical(dialect, &cases);
    }
    assert_canonical(&AnsiDialect {}, &[("b + a = 1", "((A + B) = 1)")]);
    // SQL Server also concatenates strings with `+`, so dialects without a numeric `+` keep
    // the written order.
    assert_canonical(&GenericDialect {}, &[("b + a = 1", "((b + a) = 1)")]);
}

#[test]
fn field_access_on_a_literal_is_refused() {
    // `0 .l` prints as `0.l`, which a tokenizer reads as the number `0.` and a name.
    assert_unsupported(
        &MySqlDialect {},
        &["0 .l = 1", "0 .l.0. = 1", "a.l.0. = 1", "'x'.f = 1"],
    );
}

#[test]
fn equality_between_two_columns_keeps_its_order_where_the_left_collation_wins() {
    // SQLite compares `a = b` under `a`'s collation and `b = a` under `b`'s.
    for dialect in [&SQLiteDialect {} as &dyn Dialect, &GenericDialect {}] {
        assert_canonical(
            dialect,
            &[
                ("b = a", "(b = a)"),
                ("a = b", "(a = b)"),
                ("b <> a", "(b != a)"),
                ("b IS DISTINCT FROM a", "(b IS DISTINCT FROM a)"),
                ("x = 1", "(1 = x)"),
                ("1 = x", "(1 = x)"),
                ("a IS NOT DISTINCT FROM 1", "(1 IS NOT DISTINCT FROM a)"),
            ],
        );
    }
    assert_canonical(&PostgreSqlDialect {}, &[("b = a", "(a = b)")]);
}

#[test]
fn sqlite_is_and_is_not_share_the_distinct_from_spellings() {
    assert_canonical(
        &SQLiteDialect {},
        &[
            ("b IS a", "(b IS NOT DISTINCT FROM a)"),
            ("b IS NOT DISTINCT FROM a", "(b IS NOT DISTINCT FROM a)"),
            ("b IS NOT a", "(b IS DISTINCT FROM a)"),
            ("a IS 1", "(1 IS NOT DISTINCT FROM a)"),
            ("1 IS NOT a", "(1 IS DISTINCT FROM a)"),
            ("a IS b + 1", "(a IS NOT DISTINCT FROM (1 + b))"),
            ("(a IS b) = c", "((a IS NOT DISTINCT FROM b) = c)"),
            // `UNKNOWN` is no SQLite keyword, so this compares with a column.
            ("flag IS UNKNOWN", "(flag IS NOT DISTINCT FROM unknown)"),
        ],
    );
}

#[test]
fn a_boolean_is_no_literal_where_true_may_name_a_column() {
    // SQLite reads `TRUE` as a column named `true` when the table has one, and that column
    // brings its collation to the comparison.
    assert_canonical(
        &SQLiteDialect {},
        &[
            ("TRUE < b", "(true < b)"),
            ("b > TRUE", "(b > true)"),
            ("+TRUE < b", "(+ true < b)"),
            ("b = FALSE", "(b = false)"),
        ],
    );
    assert_canonical(
        &PostgreSqlDialect {},
        &[("TRUE < b", "(b > true)"), ("b > TRUE", "(b > true)")],
    );
}
