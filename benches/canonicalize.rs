use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqlparser::ast::{SetExpr, Statement};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser_canonicalize::{Canonicalizer, hash_canonical};

/// One case per predicate shape, so a regression names the shape that caused it.
const CORPUS: &[(&str, &str)] = &[
    ("no_filter", "SELECT * FROM t"),
    ("comparison", "SELECT * FROM t WHERE age > 18"),
    ("and_pair", "SELECT * FROM t WHERE a = 1 AND b = 2"),
    ("or_triple", "SELECT * FROM t WHERE a = 1 OR b = 2 OR c = 3"),
    ("in_small", "SELECT * FROM t WHERE x IN (3, 1, 2)"),
    ("between", "SELECT * FROM t WHERE age BETWEEN 18 AND 65"),
    ("like", "SELECT * FROM t WHERE name LIKE 'A%'"),
    (
        "quoted_identifier",
        "SELECT * FROM t WHERE \"Status\" = 'paid'",
    ),
    (
        "in_subquery",
        "SELECT * FROM t WHERE x IN (SELECT id FROM m WHERE owner = 'a')",
    ),
    ("aggregate", "SELECT SUM(amount) FROM t WHERE amount > 10"),
    (
        "group_by_having",
        "SELECT region, SUM(amount) FROM orders WHERE active = TRUE GROUP BY region HAVING COUNT(*) > 2",
    ),
];

/// Sorting operands is the only super-linear work in the crate, so it gets the widest inputs.
fn wide_in_list(items: usize) -> String {
    let values: Vec<String> = (0..items).map(|item| (items - item).to_string()).collect();
    format!("SELECT * FROM t WHERE x IN ({})", values.join(", "))
}

/// Short names keep the widest chain within the crate's 8192-byte input limit.
fn long_boolean_chain(terms: usize) -> String {
    let predicates: Vec<String> = (0..terms)
        .map(|term| format!("c{:03} = {}", terms - term, term))
        .collect();
    format!("SELECT * FROM t WHERE {}", predicates.join(" AND "))
}

fn where_expr(sql: &str) -> sqlparser::ast::Expr {
    let statement = Parser::parse_sql(&PostgreSqlDialect {}, sql)
        .expect("benchmark SQL parses")
        .pop()
        .expect("benchmark SQL has one statement");
    let Statement::Query(query) = statement else {
        panic!("benchmark SQL is a query");
    };
    let SetExpr::Select(select) = *query.body else {
        panic!("benchmark SQL is a plain SELECT");
    };
    select.selection.expect("benchmark SQL has a WHERE clause")
}

fn shapes(c: &mut Criterion) {
    let dialect = PostgreSqlDialect {};
    let mut group = c.benchmark_group("normalize_sql");
    for (name, sql) in CORPUS {
        group.bench_function(*name, |b| {
            b.iter(|| {
                black_box(
                    Canonicalizer::new(&dialect)
                        .normalize_sql(black_box(sql))
                        .unwrap(),
                )
            });
        });
    }
    group.finish();
}

fn sorting(c: &mut Criterion) {
    let dialect = PostgreSqlDialect {};
    let mut group = c.benchmark_group("sorted_operands");
    for items in [8usize, 64, 512] {
        let sql = wide_in_list(items);
        group.bench_with_input(BenchmarkId::new("in_list", items), &sql, |b, sql| {
            b.iter(|| {
                black_box(
                    Canonicalizer::new(&dialect)
                        .normalize_sql(black_box(sql))
                        .unwrap(),
                )
            });
        });
        let sql = long_boolean_chain(items);
        group.bench_with_input(BenchmarkId::new("and_chain", items), &sql, |b, sql| {
            b.iter(|| {
                black_box(
                    Canonicalizer::new(&dialect)
                        .normalize_sql(black_box(sql))
                        .unwrap(),
                )
            });
        });
    }
    group.finish();
}

/// Separates the two costs `normalize_sql` pays: parsing the input, and the canonicalization
/// that follows, which includes parsing the canonical text back to prove it reads as itself.
fn entry_points(c: &mut Criterion) {
    let dialect = PostgreSqlDialect {};
    let sql = "SELECT * FROM t WHERE (a = 1 AND b = 2) OR (c = 3 AND d = 4) AND e IN (5, 6, 7)";
    let expr = where_expr(sql);
    let canonical = Canonicalizer::new(&dialect).normalize_sql(sql).unwrap();

    let mut group = c.benchmark_group("entry_points");
    group.bench_function("normalize_sql", |b| {
        b.iter(|| {
            black_box(
                Canonicalizer::new(&dialect)
                    .normalize_sql(black_box(sql))
                    .unwrap(),
            )
        });
    });
    group.bench_function("normalize_where_clause", |b| {
        b.iter(|| {
            black_box(
                Canonicalizer::new(&dialect)
                    .normalize_where_clause(Some(black_box(&expr)))
                    .unwrap(),
            )
        });
    });
    group.bench_function("parse_only", |b| {
        b.iter(|| black_box(Parser::parse_sql(&dialect, black_box(sql)).unwrap()));
    });
    group.bench_function("hash_canonical", |b| {
        b.iter(|| black_box(hash_canonical(black_box(&canonical))));
    });
    group.finish();
}

criterion_group!(benches, shapes, sorting, entry_points);
criterion_main!(benches);
