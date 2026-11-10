use core::ptr;

use sqlparser::ast::{
    DataType, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Ident, SelectItem, SetExpr,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser_canonicalize::{
    CanonicalizeError, Canonicalizer, ComparisonCoercions, SemanticContext, hash_canonical,
};

struct Evidence<'a> {
    root: &'a Expr,
    columns: Vec<(&'a Expr, Vec<Ident>)>,
    comparisons: Vec<(&'a Expr, ComparisonCoercions<'a>)>,
    strict: bool,
}

impl<'a> Evidence<'a> {
    fn empty(root: &'a Expr) -> Self {
        Self {
            root,
            columns: Vec::new(),
            comparisons: Vec::new(),
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
                right: Some(&text),
            },
        )],
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
        strict: true,
    };
    let source = normalize(&source_context).unwrap();
    let catalog = normalize(&catalog_context).unwrap();
    assert_eq!(source, "(lower(dm.role) = CAST('admin' AS TEXT))");
    assert_eq!(source, catalog);
    assert_eq!(hash_canonical(&source), hash_canonical(&catalog));
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
                left: Some(&numeric),
                right: None,
            },
        )],
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
                left: Some(&target),
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
                left: Some(&target),
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
    let root = parse("weight > 0");
    let context = Evidence {
        strict: true,
        ..Evidence::empty(&root)
    };
    assert_eq!(
        normalize(&context),
        Err(CanonicalizeError::Unsupported(
            "foreign comparison occurrence".into()
        ))
    );
}
