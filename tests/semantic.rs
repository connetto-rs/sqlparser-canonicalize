use core::ptr;

use sqlparser::ast::{
    BinaryOperator, DataType, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Ident,
    InTarget, SelectItem, SetExpr,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser_canonicalize::{
    ArrayCoercions, CanonicalizeError, Canonicalizer, ComparisonCoercions, Conversion,
    MembershipCoercions, MembershipElement, SemanticContext, hash_canonical,
};

struct Evidence<'a> {
    root: &'a Expr,
    columns: Vec<(&'a Expr, Vec<Ident>)>,
    comparisons: Vec<(&'a Expr, ComparisonCoercions<'a>)>,
    memberships: Vec<(&'a Expr, MembershipCoercions<'a>)>,
    strict: bool,
}

impl<'a> Evidence<'a> {
    fn empty(root: &'a Expr) -> Self {
        Self {
            root,
            columns: Vec::new(),
            comparisons: Vec::new(),
            memberships: Vec::new(),
            strict: false,
        }
    }
}

impl SemanticContext for Evidence<'_> {
    fn expression(&self) -> &Expr {
        self.root
    }

    fn column_reference(&self, occurrence: &Expr) -> Result<Option<&[Ident]>, CanonicalizeError> {
        if let Some((_, reference)) = self
            .columns
            .iter()
            .find(|(node, _)| ptr::eq(*node, occurrence))
        {
            return Ok(Some(reference));
        }
        if self.strict {
            return Err(CanonicalizeError::Unsupported(
                "foreign column occurrence".into(),
            ));
        }
        Ok(None)
    }

    fn comparison_coercions(
        &self,
        occurrence: &Expr,
    ) -> Result<Option<ComparisonCoercions<'_>>, CanonicalizeError> {
        if let Some((_, coercions)) = self
            .comparisons
            .iter()
            .find(|(node, _)| ptr::eq(*node, occurrence))
        {
            return Ok(Some(ComparisonCoercions {
                left: coercions.left,
                right: coercions.right,
            }));
        }
        if self.strict {
            return Err(CanonicalizeError::Unsupported(
                "foreign comparison occurrence".into(),
            ));
        }
        Ok(None)
    }

    fn membership_coercions(
        &self,
        occurrence: &Expr,
    ) -> Result<Option<MembershipCoercions<'_>>, CanonicalizeError> {
        if let Some((_, membership)) = self
            .memberships
            .iter()
            .find(|(node, _)| ptr::eq(*node, occurrence))
        {
            return Ok(Some(*membership));
        }
        if self.strict {
            return Err(CanonicalizeError::Unsupported(
                "foreign membership occurrence".into(),
            ));
        }
        Ok(None)
    }
}

fn parse(sql: &str) -> Expr {
    Parser::new(&PostgreSqlDialect {})
        .try_with_sql(sql)
        .unwrap()
        .parse_expr()
        .unwrap()
}

fn data_type(sql: &str) -> DataType {
    Parser::new(&PostgreSqlDialect {})
        .try_with_sql(sql)
        .unwrap()
        .parse_data_type()
        .unwrap()
}

fn operands(expr: &Expr) -> (&Expr, &Expr) {
    let Expr::BinaryOp { left, right, .. } = expr else {
        panic!("expected comparison")
    };
    (left, right)
}

fn argument(expr: &Expr) -> &Expr {
    let Expr::Function(function) = expr else {
        panic!("expected function")
    };
    let FunctionArguments::List(arguments) = &function.args else {
        panic!("expected arguments")
    };
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(argument)) = &arguments.args[0] else {
        panic!("expected argument")
    };
    argument
}

fn projection(expr: &Expr) -> &Expr {
    let Expr::Subquery(query) = expr else {
        panic!("expected scalar query")
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        panic!("expected select")
    };
    match &select.projection[0] {
        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => expr,
        _ => panic!("expected projection expression"),
    }
}

fn reference(parts: &[&str]) -> Vec<Ident> {
    parts.iter().map(|part| Ident::new(*part)).collect()
}

fn normalize(context: &dyn SemanticContext) -> Result<String, CanonicalizeError> {
    Canonicalizer::new(&PostgreSqlDialect {}).normalize_with_context(context)
}

#[test]
fn proved_text_comparison_materializes_the_literal_conversion() {
    let source = parse("lower(role) = 'admin'");
    let catalog = parse("lower(role) = 'admin'::TEXT");
    let text = data_type("TEXT");
    let source_context = Evidence {
        root: &source,
        columns: vec![(argument(operands(&source).0), reference(&["dm", "role"]))],
        comparisons: vec![(
            &source,
            ComparisonCoercions {
                left: None,
                right: Some(Conversion::Cast(&text)),
            },
        )],
        memberships: Vec::new(),
        strict: true,
    };
    let catalog_context = Evidence {
        root: &catalog,
        columns: vec![(argument(operands(&catalog).0), reference(&["dm", "role"]))],
        comparisons: vec![(
            &catalog,
            ComparisonCoercions {
                left: None,
                right: None,
            },
        )],
        memberships: Vec::new(),
        strict: true,
    };
    let source = normalize(&source_context).unwrap();
    let catalog = normalize(&catalog_context).unwrap();
    assert_eq!(source, "(lower(dm.role) = CAST('admin' AS TEXT))");
    assert_eq!(source, catalog);
    assert_eq!(hash_canonical(&source), hash_canonical(&catalog));
}

#[test]
fn check_constraint_matches_postgresql_deparse_of_a_text_comparison() {
    let declared = parse("s <> ''");
    let deparsed = parse("(s <> ''::TEXT)");
    let Expr::Nested(deparsed_comparison) = &deparsed else {
        panic!("expected parenthesized comparison")
    };
    let text = data_type("TEXT");
    let declared_context = Evidence {
        comparisons: vec![(
            &declared,
            ComparisonCoercions {
                left: None,
                right: Some(Conversion::Cast(&text)),
            },
        )],
        ..Evidence::empty(&declared)
    };
    let deparsed_context = Evidence {
        comparisons: vec![(
            deparsed_comparison,
            ComparisonCoercions {
                left: None,
                right: None,
            },
        )],
        ..Evidence::empty(&deparsed)
    };
    let declared = normalize(&declared_context).unwrap();
    assert_eq!(declared, "(s != CAST('' AS TEXT))");
    assert_eq!(declared, normalize(&deparsed_context).unwrap());
}

/// Canonicalizes `declared` with `membership` proved for its root `IN` list and asserts that
/// PostgreSQL's deparse of it, read without any proof, has the same canonical text.
fn reconcile_membership(
    declared: &str,
    membership: MembershipCoercions<'_>,
    deparsed: &str,
) -> String {
    let declared = parse(declared);
    let context = Evidence {
        memberships: vec![(&declared, membership)],
        ..Evidence::empty(&declared)
    };
    let canonical = normalize(&context).unwrap();
    let deparsed = parse(deparsed);
    assert_eq!(canonical, normalize(&Evidence::empty(&deparsed)).unwrap());
    canonical
}

fn in_array(conversion: Option<Conversion<'_>>) -> MembershipElement<'_> {
    MembershipElement::Array(conversion)
}

fn compared<'a>(
    left: Option<Conversion<'a>>,
    right: Option<Conversion<'a>>,
) -> MembershipElement<'a> {
    MembershipElement::Comparison(ComparisonCoercions { left, right })
}

const PLAIN_ARRAY: Option<ArrayCoercions<'static>> = Some(ArrayCoercions {
    left: None,
    array: None,
});

#[test]
fn proved_in_lists_match_postgresql_array_comparisons() {
    let text = data_type("TEXT");
    let varchar = data_type("CHARACTER VARYING");
    let text_array = data_type("TEXT[]");
    let bpchar = data_type("bpchar");
    let (one, two, minus_one) = (parse("1"), parse("2"), parse("'-1'::INTEGER"));
    let to_text = Some(Conversion::Cast(&text));
    let text_pair = [in_array(to_text), in_array(to_text)];

    assert_eq!(
        reconcile_membership(
            "s IN ('a', 'b')",
            MembershipCoercions {
                array: PLAIN_ARRAY,
                elements: &text_pair,
            },
            "(s = ANY (ARRAY['a'::text, 'b'::text]))",
        ),
        "s = ANY(ARRAY[CAST('a' AS TEXT), CAST('b' AS TEXT)])"
    );
    assert_eq!(
        reconcile_membership(
            "s NOT IN ('a', 'b')",
            MembershipCoercions {
                array: PLAIN_ARRAY,
                elements: &text_pair,
            },
            "(s <> ALL (ARRAY['a'::text, 'b'::text]))",
        ),
        "s != ALL(ARRAY[CAST('a' AS TEXT), CAST('b' AS TEXT)])"
    );
    let to_varchar = Some(Conversion::Cast(&varchar));
    assert_eq!(
        reconcile_membership(
            "v IN ('a', 'b')",
            MembershipCoercions {
                array: Some(ArrayCoercions {
                    left: to_text,
                    array: Some(&text_array),
                }),
                elements: &[in_array(to_varchar), in_array(to_varchar)],
            },
            "((v)::text = ANY ((ARRAY['a'::character varying, 'b'::character varying])::text[]))",
        ),
        "CAST(v AS TEXT) = ANY(CAST(ARRAY[CAST('a' AS CHARACTER VARYING), CAST('b' AS CHARACTER VARYING)] AS TEXT[]))"
    );
    let to_bpchar = Some(Conversion::Cast(&bpchar));
    reconcile_membership(
        "c IN ('a', 'b')",
        MembershipCoercions {
            array: PLAIN_ARRAY,
            elements: &[in_array(to_bpchar), in_array(to_bpchar)],
        },
        "(c = ANY (ARRAY['a'::bpchar, 'b'::bpchar]))",
    );
    reconcile_membership(
        "s IN ('a', 'b', NULL)",
        MembershipCoercions {
            array: PLAIN_ARRAY,
            elements: &[in_array(to_text), in_array(to_text), in_array(to_text)],
        },
        "(s = ANY (ARRAY['a'::text, 'b'::text, NULL::text]))",
    );
    assert_eq!(
        reconcile_membership(
            "i IN (1, 2)",
            MembershipCoercions {
                array: PLAIN_ARRAY,
                elements: &[in_array(None), in_array(None)],
            },
            "(i = ANY (ARRAY[1, 2]))",
        ),
        "i = ANY(ARRAY[1, 2])"
    );
    reconcile_membership(
        "i IN ('1', '2')",
        MembershipCoercions {
            array: PLAIN_ARRAY,
            elements: &[
                in_array(Some(Conversion::Constant(&one))),
                in_array(Some(Conversion::Constant(&two))),
            ],
        },
        "(i = ANY (ARRAY[1, 2]))",
    );
    assert_eq!(
        reconcile_membership(
            "i IN (-1, 2)",
            MembershipCoercions {
                array: PLAIN_ARRAY,
                elements: &[
                    in_array(Some(Conversion::Constant(&minus_one))),
                    in_array(None)
                ],
            },
            "(i = ANY (ARRAY['-1'::integer, 2]))",
        ),
        "i = ANY(ARRAY[CAST('-1' AS INTEGER), 2])"
    );
}

#[test]
fn proved_in_lists_match_postgresql_comparison_chains() {
    let text = data_type("TEXT");
    let seven = parse("7");
    let to_text = Some(Conversion::Cast(&text));
    assert_eq!(
        reconcile_membership(
            "s IN ('a')",
            MembershipCoercions {
                array: None,
                elements: &[compared(None, to_text)],
            },
            "(s = 'a'::text)",
        ),
        "(s = CAST('a' AS TEXT))"
    );
    reconcile_membership(
        "i IN ('7')",
        MembershipCoercions {
            array: None,
            elements: &[compared(None, Some(Conversion::Constant(&seven)))],
        },
        "(i = 7)",
    );
    assert_eq!(
        reconcile_membership(
            "s IN ('a', u, w)",
            MembershipCoercions {
                array: None,
                elements: &[
                    compared(None, to_text),
                    compared(None, None),
                    compared(None, None)
                ],
            },
            "(((s = 'a'::text) OR (s = u)) OR (s = w))",
        ),
        "(((s = CAST('a' AS TEXT)) OR (s = u)) OR (s = w))"
    );
    assert_eq!(
        reconcile_membership(
            "s IN ('a', 'b', u, 'c', w)",
            MembershipCoercions {
                array: PLAIN_ARRAY,
                elements: &[
                    in_array(to_text),
                    in_array(to_text),
                    compared(None, None),
                    in_array(to_text),
                    compared(None, None),
                ],
            },
            "(((s = ANY (ARRAY['a'::text, 'b'::text, 'c'::text])) OR (s = u)) OR (s = w))",
        ),
        "(((s = ANY(ARRAY[CAST('a' AS TEXT), CAST('b' AS TEXT), CAST('c' AS TEXT)])) OR (s = u)) OR (s = w))"
    );
    assert_eq!(
        reconcile_membership(
            "s NOT IN ('a', u)",
            MembershipCoercions {
                array: None,
                elements: &[compared(None, to_text), compared(None, None)],
            },
            "((s <> 'a'::text) AND (s <> u))",
        ),
        "((s != CAST('a' AS TEXT)) AND (s != u))"
    );
    assert_eq!(
        reconcile_membership(
            "v IN ('a', u)",
            MembershipCoercions {
                array: None,
                elements: &[compared(to_text, to_text), compared(to_text, None)],
            },
            "(((v)::text = 'a'::text) OR ((v)::text = u))",
        ),
        "((CAST(v AS TEXT) = CAST('a' AS TEXT)) OR (CAST(v AS TEXT) = u))"
    );
    reconcile_membership(
        "i IN (j, 1)",
        MembershipCoercions {
            array: None,
            elements: &[compared(None, None), compared(None, None)],
        },
        "((i = j) OR (i = 1))",
    );
}

#[test]
fn a_proved_in_list_nested_in_a_check_keeps_the_deparsed_grouping() {
    let text = data_type("TEXT");
    let to_text = Some(Conversion::Cast(&text));
    let elements = [in_array(to_text), in_array(to_text)];
    let declared = parse("v > 0 AND s IN ('a', 'b')");
    let (_, membership) = operands(&declared);
    let context = Evidence {
        memberships: vec![(
            membership,
            MembershipCoercions {
                array: PLAIN_ARRAY,
                elements: &elements,
            },
        )],
        ..Evidence::empty(&declared)
    };
    let deparsed = parse("((v > 0) AND (s = ANY (ARRAY['a'::text, 'b'::text])))");
    let declared = normalize(&context).unwrap();
    assert_eq!(
        declared,
        "((v > 0) AND (s = ANY(ARRAY[CAST('a' AS TEXT), CAST('b' AS TEXT)])))"
    );
    assert_eq!(declared, normalize(&Evidence::empty(&deparsed)).unwrap());
}

#[test]
fn a_folded_literal_comparison_matches_its_deparse() {
    let declared = parse("i = '7'");
    let seven = parse("7");
    let context = Evidence {
        comparisons: vec![(
            &declared,
            ComparisonCoercions {
                left: None,
                right: Some(Conversion::Constant(&seven)),
            },
        )],
        ..Evidence::empty(&declared)
    };
    let deparsed = parse("(i = 7)");
    let declared = normalize(&context).unwrap();
    assert_eq!(declared, "(i = 7)");
    assert_eq!(declared, normalize(&Evidence::empty(&deparsed)).unwrap());
}

#[test]
fn a_conversion_goes_under_the_collation_as_postgresql_applies_it() {
    let text = data_type("TEXT");
    let to_text = Some(Conversion::Cast(&text));
    let one = parse("1");
    let to_one = Some(Conversion::Constant(&one));
    for (declared, left, right, deparsed, canonical) in [
        (
            "s = 'a' COLLATE \"C\"",
            None,
            to_text,
            "(s = ('a'::text COLLATE \"C\"))",
            "(s = (CAST('a' AS TEXT) COLLATE \"C\"))",
        ),
        (
            "s = (('a') COLLATE \"C\")",
            None,
            to_text,
            "(s = ('a'::text COLLATE \"C\"))",
            "(s = (CAST('a' AS TEXT) COLLATE \"C\"))",
        ),
        (
            "vc = ('a' COLLATE \"C\")",
            to_text,
            to_text,
            "((vc)::text = ('a'::text COLLATE \"C\"))",
            "(CAST(vc AS TEXT) = (CAST('a' AS TEXT) COLLATE \"C\"))",
        ),
        ("v = '1' COLLATE \"C\"", None, to_one, "(v = 1)", "(v = 1)"),
    ] {
        let declared = parse(declared);
        let context = Evidence {
            comparisons: vec![(&declared, ComparisonCoercions { left, right })],
            ..Evidence::empty(&declared)
        };
        let declared = normalize(&context).unwrap();
        assert_eq!(declared, canonical);
        let deparsed = parse(deparsed);
        assert_eq!(declared, normalize(&Evidence::empty(&deparsed)).unwrap());
    }
}

#[test]
fn an_in_list_element_converts_under_its_collation() {
    let text = data_type("TEXT");
    let to_text = Some(Conversion::Cast(&text));
    let (one, two) = (parse("1"), parse("2"));
    assert_eq!(
        reconcile_membership(
            "s IN ('a' COLLATE \"C\", 'b')",
            MembershipCoercions {
                array: PLAIN_ARRAY,
                elements: &[in_array(to_text), in_array(to_text)],
            },
            "(s = ANY (ARRAY[('a'::text COLLATE \"C\"), 'b'::text]))",
        ),
        "s = ANY(ARRAY[CAST('a' AS TEXT) COLLATE \"C\", CAST('b' AS TEXT)])"
    );
    reconcile_membership(
        "v IN ('1' COLLATE \"C\", '2')",
        MembershipCoercions {
            array: PLAIN_ARRAY,
            elements: &[
                in_array(Some(Conversion::Constant(&one))),
                in_array(Some(Conversion::Constant(&two))),
            ],
        },
        "(v = ANY (ARRAY[1, 2]))",
    );
}

#[test]
fn inconsistent_membership_proofs_are_refused() {
    let text = data_type("TEXT");
    let one = parse("1");
    let to_text = Some(Conversion::Cast(&text));
    let root = parse("s IN ('a', u)");
    let empty = Expr::In {
        expr: Box::new(parse("s")),
        target: InTarget::List(Vec::new()),
        negated: false,
        global: false,
    };
    for (root, membership) in [
        (
            &root,
            MembershipCoercions {
                array: None,
                elements: &[compared(None, to_text)],
            },
        ),
        (
            &root,
            MembershipCoercions {
                array: None,
                elements: &[in_array(to_text), compared(None, None)],
            },
        ),
        (
            &root,
            MembershipCoercions {
                array: PLAIN_ARRAY,
                elements: &[compared(None, to_text), compared(None, None)],
            },
        ),
        (
            &root,
            MembershipCoercions {
                array: None,
                elements: &[
                    compared(None, to_text),
                    compared(None, Some(Conversion::Constant(&one))),
                ],
            },
        ),
        (
            &empty,
            MembershipCoercions {
                array: None,
                elements: &[],
            },
        ),
    ] {
        let context = Evidence {
            memberships: vec![(root, membership)],
            ..Evidence::empty(root)
        };
        assert!(matches!(
            normalize(&context),
            Err(CanonicalizeError::Unsupported(_))
        ));
    }
}

#[test]
fn folding_a_column_into_a_constant_is_refused() {
    let root = parse("i = j");
    let one = parse("1");
    let fold = Some(Conversion::Constant(&one));
    for (left, right) in [(fold, None), (None, fold)] {
        let context = Evidence {
            comparisons: vec![(&root, ComparisonCoercions { left, right })],
            ..Evidence::empty(&root)
        };
        assert!(matches!(
            normalize(&context),
            Err(CanonicalizeError::Unsupported(_))
        ));
    }
}

#[test]
fn deeply_parenthesized_folded_literals_do_not_exhaust_the_stack() {
    fn parenthesized(depth: usize) -> Expr {
        let mut expr = parse("'1'");
        for _ in 0..depth {
            expr = Expr::Nested(Box::new(expr));
        }
        expr
    }
    let seven = parse("7");
    let unfoldable = parse("i = j");
    let deep_constant = parenthesized(1_000_000);
    let deep_operand = Expr::BinaryOp {
        left: Box::new(parse("i")),
        op: BinaryOperator::Eq,
        right: Box::new(parenthesized(1_000_000)),
    };
    let fold = |root, constant| {
        normalize(&Evidence {
            comparisons: vec![(
                root,
                ComparisonCoercions {
                    left: None,
                    right: Some(Conversion::Constant(constant)),
                },
            )],
            ..Evidence::empty(root)
        })
    };
    assert!(matches!(
        fold(&unfoldable, &deep_constant),
        Err(CanonicalizeError::Unsupported(_))
    ));
    assert_eq!(fold(&deep_operand, &seven), Ok("(i = 7)".to_string()));
    // Dropping a million nested boxes recurses once per level.
    core::mem::forget((deep_constant, deep_operand));
}

#[test]
fn proved_numeric_comparison_keeps_numeric_conversion_and_scoped_columns() {
    let source = parse("weight > (SELECT avg(weight) FROM \"public\".\"paper_shares\")");
    let catalog = parse(
        "(weight)::NUMERIC > (SELECT avg(paper_shares.weight) AS avg FROM \"public\".\"paper_shares\")",
    );
    let numeric = data_type("NUMERIC");
    let (source_weight, source_query) = operands(&source);
    let (catalog_cast, catalog_query) = operands(&catalog);
    let Expr::Cast {
        expr: catalog_weight,
        ..
    } = catalog_cast
    else {
        panic!("expected cast")
    };
    let Expr::Nested(catalog_weight) = catalog_weight.as_ref() else {
        panic!("expected nested column")
    };
    let source_context = Evidence {
        root: &source,
        columns: vec![
            (source_weight, reference(&["s", "weight"])),
            (
                argument(projection(source_query)),
                reference(&["paper_shares", "weight"]),
            ),
        ],
        comparisons: vec![(
            &source,
            ComparisonCoercions {
                left: Some(Conversion::Cast(&numeric)),
                right: None,
            },
        )],
        memberships: Vec::new(),
        strict: true,
    };
    let catalog_context = Evidence {
        root: &catalog,
        columns: vec![
            (catalog_weight, reference(&["s", "weight"])),
            (
                argument(projection(catalog_query)),
                reference(&["paper_shares", "weight"]),
            ),
        ],
        comparisons: vec![(
            &catalog,
            ComparisonCoercions {
                left: None,
                right: None,
            },
        )],
        memberships: Vec::new(),
        strict: true,
    };
    let source = normalize(&source_context).unwrap();
    let catalog = normalize(&catalog_context).unwrap();
    assert_eq!(
        source,
        "(CAST(s.weight AS NUMERIC) > (SELECT avg(paper_shares.weight) FROM \"public\".paper_shares))"
    );
    assert_eq!(source, catalog);
    assert_eq!(hash_canonical(&source), hash_canonical(&catalog));
}

#[test]
fn absent_evidence_does_not_erase_explicit_casts() {
    let source = parse("lower(role) = 'admin'");
    let catalog = parse("lower(role) = 'admin'::TEXT");
    let source = normalize(&Evidence::empty(&source)).unwrap();
    let catalog = normalize(&Evidence::empty(&catalog)).unwrap();
    assert_ne!(source, catalog);
    assert_ne!(hash_canonical(&source), hash_canonical(&catalog));
    assert!(catalog.contains("CAST('admin' AS TEXT)"));
}

#[test]
fn semantic_rendering_preserves_operation_order_and_boolean_grouping() {
    for (left, right) in [
        ("a = b", "b = a"),
        ("a > b", "b < a"),
        ("a + b", "b + a"),
        ("a * b", "b * a"),
        ("a AND (b AND c)", "(a AND b) AND c"),
        ("a OR b", "b OR a"),
        ("a IN (3, 1, 2)", "a IN (1, 2, 3)"),
        ("a IS DISTINCT FROM b", "b IS DISTINCT FROM a"),
    ] {
        let left = parse(left);
        let right = parse(right);
        assert_ne!(
            normalize(&Evidence::empty(&left)).unwrap(),
            normalize(&Evidence::empty(&right)).unwrap()
        );
    }
}

#[test]
fn qualified_type_and_typmod_survive_materialization() {
    let root = parse("weight > 0");
    let target = data_type("\"types\".\"Measure\"(12, 3)");
    let context = Evidence {
        comparisons: vec![(
            &root,
            ComparisonCoercions {
                left: Some(Conversion::Cast(&target)),
                right: None,
            },
        )],
        ..Evidence::empty(&root)
    };
    assert_eq!(
        normalize(&context).unwrap(),
        "(CAST(weight AS \"types\".\"Measure\"(12, 3)) > 0)"
    );
}

#[test]
fn materializing_a_coercion_keeps_an_existing_cast() {
    let root = parse("weight::INT > 0");
    let target = data_type("NUMERIC");
    let context = Evidence {
        comparisons: vec![(
            &root,
            ComparisonCoercions {
                left: Some(Conversion::Cast(&target)),
                right: None,
            },
        )],
        ..Evidence::empty(&root)
    };
    assert_eq!(
        normalize(&context).unwrap(),
        "(CAST(CAST(weight AS INT) AS NUMERIC) > 0)"
    );
}

#[test]
fn a_shadowed_occurrence_cannot_reuse_another_columns_spelling() {
    let root = parse("weight > (SELECT avg(weight) FROM paper_shares)");
    let (outer, query) = operands(&root);
    let inner = argument(projection(query));
    let context = Evidence {
        columns: vec![
            (outer, reference(&["outer_scope", "weight"])),
            (inner, reference(&["paper_shares", "weight"])),
        ],
        ..Evidence::empty(&root)
    };
    let text = normalize(&context).unwrap();
    assert_eq!(
        text,
        "(outer_scope.weight > (SELECT avg(paper_shares.weight) FROM paper_shares))"
    );
}

#[test]
fn projection_aliases_with_output_name_consumers_are_refused() {
    for sql in [
        "x > (SELECT avg(weight) AS avg FROM paper_shares ORDER BY avg)",
        "x > (SELECT weight AS w FROM paper_shares GROUP BY w)",
        "x > (SELECT weight AS w FROM paper_shares HAVING w > 1)",
    ] {
        let root = parse(sql);
        assert!(matches!(
            normalize(&Evidence::empty(&root)),
            Err(CanonicalizeError::Unsupported(_))
        ));
    }
}

#[test]
fn schema_free_rendering_does_not_adopt_semantic_alias_elision() {
    let root = parse("weight > (SELECT avg(weight) AS avg FROM paper_shares)");
    assert!(matches!(
        Canonicalizer::new(&PostgreSqlDialect {}).normalize_where_clause(Some(&root)),
        Err(CanonicalizeError::Unsupported(_))
    ));
    assert_eq!(
        normalize(&Evidence::empty(&root)).unwrap(),
        "(weight > (SELECT avg(weight) FROM paper_shares))"
    );
}

#[test]
fn provider_errors_are_not_silently_treated_as_missing_proofs() {
    for (sql, error) in [
        ("weight > 0", "foreign comparison occurrence"),
        ("weight IN (1, 2)", "foreign membership occurrence"),
    ] {
        let root = parse(sql);
        let context = Evidence {
            strict: true,
            ..Evidence::empty(&root)
        };
        assert_eq!(
            normalize(&context),
            Err(CanonicalizeError::Unsupported(error.into()))
        );
    }
}

#[test]
fn empty_proved_column_references_are_refused() {
    let root = parse("weight");
    let context = Evidence {
        columns: vec![(&root, Vec::new())],
        ..Evidence::empty(&root)
    };
    assert!(matches!(
        normalize(&context),
        Err(CanonicalizeError::Unsupported(_))
    ));
}

#[test]
fn unquotable_column_and_field_roots_are_refused() {
    let column = || Expr::Identifier(Ident::new("not"));
    let canonicalizer = Canonicalizer::new(&sqlparser::dialect::GenericDialect {});
    for root in [
        column(),
        Expr::CompoundFieldAccess {
            root: Box::new(column()),
            access_chain: vec![sqlparser::ast::AccessExpr::Dot(Expr::Identifier(
                Ident::new("field"),
            ))],
        },
    ] {
        assert!(matches!(
            canonicalizer.normalize_with_context(&Evidence::empty(&root)),
            Err(CanonicalizeError::Unsupported(_))
        ));
    }
}
