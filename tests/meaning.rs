//! Generated predicates, checked against the meaning oracle in every dialect.

mod oracle;

use oracle::{Folding, meaning};
use sqlparser::ast::Expr;
use sqlparser::dialect::{
    AnsiDialect, Dialect, GenericDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect,
};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Token;
use sqlparser_canonicalize::{CanonicalizeError, Canonicalizer};

const SEEDS: u64 = 3000;

/// Xorshift, so every run generates the same predicates.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).expect("bound fits in u64");
        usize::try_from(self.next() % bound).expect("value below a usize bound fits in usize")
    }

    fn percent(&mut self, chance: usize) -> bool {
        self.below(100) < chance
    }
}

enum Tree {
    Atom(&'static str, bool),
    Binary(&'static str, Box<Tree>, Box<Tree>),
    Not(Box<Tree>),
    Negate(Box<Tree>),
    Postfix(&'static str, Box<Tree>),
    In(Box<Tree>, Vec<Tree>, bool),
    Between(Box<Tree>, Box<Tree>, Box<Tree>, bool),
    /// Operator, subject, pattern, negated, and `ANY` for `LIKE ANY` and `ILIKE ANY`.
    Like(&'static str, Box<Tree>, Box<Tree>, bool, bool),
    Quantified(&'static str, &'static str, Box<Tree>, Box<Tree>),
    AtTimeZone(Box<Tree>, Box<Tree>),
    Coalesce(Box<Tree>, Box<Tree>),
    Cast(Box<Tree>),
    Case(Box<Tree>, Box<Tree>, Option<Box<Tree>>),
    Extract(Box<Tree>),
    Substring(Box<Tree>, Box<Tree>, Box<Tree>),
    Position(Box<Tree>, Box<Tree>),
    Tuple(Box<Tree>, Box<Tree>),
    /// Subject, filter, negation and table of `x [NOT] IN (SELECT a FROM table [WHERE
    /// filter])`.
    InQuery(Box<Tree>, Option<Box<Tree>>, bool, Table),
    /// Filter, negation and table of `[NOT] EXISTS (SELECT * FROM table [WHERE filter])`.
    Exists(Option<Box<Tree>>, bool, Table),
    /// Filter and table of the scalar subquery `(SELECT b FROM table [WHERE filter])`.
    Scalar(Option<Box<Tree>>, Table),
}

/// The table a subquery reads, `[ONLY] u [v]`.
#[derive(Clone, Copy)]
struct Table {
    only: bool,
    aliased: bool,
}

impl Table {
    fn random(rng: &mut Rng) -> Self {
        Self {
            only: rng.percent(20),
            aliased: rng.percent(30),
        }
    }
}

const BINARY: &[&str] = &[
    "=",
    "!=",
    "<",
    "<=",
    ">",
    ">=",
    "+",
    "-",
    "*",
    "/",
    "%",
    "AND",
    "OR",
    "<=>",
    "IS DISTINCT FROM",
    "IS NOT DISTINCT FROM",
    "IS",
    "IS NOT",
];
/// Comparisons whose operands swap freely where the collation of two operands does not depend
/// on their order.
const SYMMETRIC_COMPARISON: &[&str] = &[
    "=",
    "!=",
    "<=>",
    "IS DISTINCT FROM",
    "IS NOT DISTINCT FROM",
    "IS",
    "IS NOT",
];
const POSTFIX: &[&str] = &[
    "IS NULL",
    "IS NOT NULL",
    "IS TRUE",
    "IS NOT TRUE",
    "IS FALSE",
    "IS NOT FALSE",
    "IS UNKNOWN",
    "IS NOT UNKNOWN",
];
const PATTERN_MATCH: &[&str] = &["LIKE", "ILIKE", "SIMILAR TO", "RLIKE", "REGEXP"];
const COMPARISON: &[&str] = &["=", "!=", "<", ">="];
const QUANTIFIER: &[&str] = &["ANY", "SOME", "ALL"];
const ATOMS: &[(&str, bool)] = &[
    ("a", true),
    ("b", true),
    ("c", true),
    ("1", false),
    ("2", false),
    ("'x'", false),
    ("NULL", false),
    ("TRUE", false),
];

/// Builds a random predicate tree.
fn tree(rng: &mut Rng, depth: u32, quoted: &'static str) -> Tree {
    if depth == 0 || rng.percent(25) {
        if rng.percent(8) {
            return Tree::Atom(quoted, false);
        }
        let (text, identifier) = ATOMS[rng.below(ATOMS.len())];
        return Tree::Atom(text, identifier);
    }
    let child = |rng: &mut Rng| Box::new(tree(rng, depth - 1, quoted));
    match rng.below(23) {
        0..=4 => Tree::Binary(BINARY[rng.below(BINARY.len())], child(rng), child(rng)),
        5 => Tree::Not(child(rng)),
        6 => Tree::Negate(child(rng)),
        7 => Tree::Postfix(POSTFIX[rng.below(POSTFIX.len())], child(rng)),
        8 => {
            let items = (0..=rng.below(3))
                .map(|_| tree(rng, depth - 1, quoted))
                .collect();
            Tree::In(child(rng), items, rng.percent(50))
        }
        9 => Tree::Between(child(rng), child(rng), child(rng), rng.percent(50)),
        10 => {
            let operator = PATTERN_MATCH[rng.below(PATTERN_MATCH.len())];
            let any = matches!(operator, "LIKE" | "ILIKE") && rng.percent(20);
            Tree::Like(operator, child(rng), child(rng), rng.percent(50), any)
        }
        11 => {
            let comparison = COMPARISON[rng.below(COMPARISON.len())];
            let quantifier = QUANTIFIER[rng.below(QUANTIFIER.len())];
            Tree::Quantified(comparison, quantifier, child(rng), child(rng))
        }
        12 => Tree::AtTimeZone(child(rng), child(rng)),
        13 => Tree::Coalesce(child(rng), child(rng)),
        14 => Tree::Cast(child(rng)),
        15 => {
            let otherwise = rng.percent(70).then(|| child(rng));
            Tree::Case(child(rng), child(rng), otherwise)
        }
        16 => Tree::Extract(child(rng)),
        17 => Tree::Substring(child(rng), child(rng), child(rng)),
        18 => Tree::Position(child(rng), child(rng)),
        19 => Tree::Tuple(child(rng), child(rng)),
        20 => {
            let filter = rng.percent(70).then(|| child(rng));
            Tree::InQuery(child(rng), filter, rng.percent(50), Table::random(rng))
        }
        21 => {
            let filter = rng.percent(70).then(|| child(rng));
            Tree::Exists(filter, rng.percent(50), Table::random(rng))
        }
        _ => {
            let filter = rng.percent(70).then(|| child(rng));
            Tree::Scalar(filter, Table::random(rng))
        }
    }
}

/// How to spell a tree. The default spelling leaves each operand bare or parenthesized at
/// random, so the dialect's precedence decides its grouping.
#[derive(Clone, Copy, Default)]
struct Spelling {
    parenthesize_every_operand: bool,
    flip_name_case: bool,
    flip_keyword_case: bool,
    swap_symmetric_operands: bool,
    /// Lets `swap_symmetric_operands` swap the operands of `=` and its kin, which keeps the
    /// comparison only where the collation of two operands does not depend on their order.
    swap_comparison_operands: bool,
    /// Spells some casts `x::T`, which only PostgreSQL among the dialects here reads.
    double_colon_cast: bool,
    /// Flips the case of the subquery table's name and alias, which only dialects that fold
    /// table names treat as the same table.
    flip_table_case: bool,
    /// Spells a missing subquery filter as `WHERE TRUE` at times, which is the same filter only
    /// where `TRUE` is reserved.
    true_filter_synonym: bool,
    /// Writes `u *` for `u` and `ONLY (u)` for `ONLY u` at times, which only dialects that
    /// read inheritance markers treat as the same table.
    inheritance_synonyms: bool,
    /// Writes `b > a` for `a < b` at times, which is one comparison where the collation of two
    /// operands does not depend on their order.
    mirror_comparisons: bool,
    /// Swaps the operands of `+` and `*` at times, which is the same value only where `+` is
    /// numeric.
    swap_numeric_operands: bool,
}

fn keyword(text: &str, spelling: Spelling, rng: &mut Rng) -> String {
    if spelling.flip_keyword_case && rng.percent(50) {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

fn spell(tree: &Tree, spelling: Spelling, rng: &mut Rng) -> String {
    let operand = |tree: &Tree, rng: &mut Rng| {
        let text = spell(tree, spelling, rng);
        if spelling.parenthesize_every_operand {
            if spelling.flip_keyword_case && rng.percent(20) {
                format!("(({text}))")
            } else {
                format!("({text})")
            }
        } else if rng.percent(40) {
            format!("({text})")
        } else {
            text
        }
    };
    match tree {
        Tree::Atom(text, identifier) => {
            if *identifier && spelling.flip_name_case && rng.percent(50) {
                text.to_uppercase()
            } else if !*identifier && text.chars().all(|c| c.is_ascii_uppercase()) {
                keyword(text, spelling, rng)
            } else {
                (*text).to_string()
            }
        }
        Tree::Binary(operator, left, right) => {
            let (mut first, mut second) = (left, right);
            let mut operator = *operator;
            let swappable = matches!(operator, "AND" | "OR")
                || (spelling.swap_comparison_operands && SYMMETRIC_COMPARISON.contains(&operator))
                || (spelling.swap_numeric_operands && matches!(operator, "+" | "*"));
            if (spelling.swap_symmetric_operands && swappable) && rng.percent(50) {
                (first, second) = (second, first);
            }
            let mirrored = match operator {
                "<" => Some(">"),
                ">" => Some("<"),
                "<=" => Some(">="),
                ">=" => Some("<="),
                _ => None,
            };
            if let Some(mirrored) =
                mirrored.filter(|_| spelling.mirror_comparisons && rng.percent(50))
            {
                operator = mirrored;
                (first, second) = (second, first);
            }
            let operator = if operator == "!=" && spelling.flip_keyword_case && rng.percent(50) {
                "<>".to_string()
            } else {
                keyword(operator, spelling, rng)
            };
            let (first, second) = (operand(first, rng), operand(second, rng));
            format!("{first} {operator} {second}")
        }
        Tree::Not(inner) => format!("{} {}", keyword("NOT", spelling, rng), operand(inner, rng)),
        Tree::Negate(inner) => format!("- {}", operand(inner, rng)),
        Tree::Postfix(operator, inner) => {
            format!(
                "{} {}",
                operand(inner, rng),
                keyword(operator, spelling, rng)
            )
        }
        Tree::In(subject, items, negated) => {
            let subject = operand(subject, rng);
            let items: Vec<String> = items.iter().map(|item| operand(item, rng)).collect();
            let not = if *negated { "NOT " } else { "" };
            let not = keyword(not, spelling, rng);
            format!("{subject} {not}IN ({})", items.join(", "))
        }
        Tree::Between(subject, low, high, negated) => {
            let (subject, low, high) =
                (operand(subject, rng), operand(low, rng), operand(high, rng));
            let not = if *negated { "NOT " } else { "" };
            format!(
                "{subject} {}BETWEEN {low} AND {high}",
                keyword(not, spelling, rng)
            )
        }
        Tree::Like(operator, subject, pattern, negated, any) => {
            let (subject, pattern) = (operand(subject, rng), operand(pattern, rng));
            let not = if *negated { "NOT " } else { "" };
            let not = keyword(not, spelling, rng);
            let any = if *any {
                keyword(" ANY", spelling, rng)
            } else {
                String::new()
            };
            format!(
                "{subject} {not}{}{any} {pattern}",
                keyword(operator, spelling, rng)
            )
        }
        Tree::Quantified(comparison, quantifier, left, right) => {
            let (left, right) = (operand(left, rng), spell(right, spelling, rng));
            let quantifier = match *quantifier {
                "ANY" | "SOME" if spelling.flip_keyword_case && rng.percent(50) => {
                    if *quantifier == "ANY" { "SOME" } else { "ANY" }
                }
                other => other,
            };
            let quantifier = keyword(quantifier, spelling, rng);
            format!("{left} {comparison} {quantifier}({right})")
        }
        Tree::AtTimeZone(timestamp, zone) => {
            let (timestamp, zone) = (operand(timestamp, rng), operand(zone, rng));
            format!(
                "{timestamp} {} {zone}",
                keyword("AT TIME ZONE", spelling, rng)
            )
        }
        // A function name is a name, so its case follows the dialect's folding.
        Tree::Coalesce(first, second) => {
            let name = if spelling.flip_name_case && rng.percent(50) {
                "coalesce"
            } else {
                "COALESCE"
            };
            format!("{name}({}, {})", operand(first, rng), operand(second, rng))
        }
        Tree::Cast(inner) => {
            let inner = operand(inner, rng);
            if spelling.double_colon_cast && rng.percent(50) {
                format!("{inner}::INTEGER")
            } else {
                format!("{}({inner} AS INTEGER)", keyword("CAST", spelling, rng))
            }
        }
        Tree::Case(condition, then, otherwise) => {
            let (condition, then) = (operand(condition, rng), operand(then, rng));
            let otherwise = match otherwise {
                Some(otherwise) => format!(" ELSE {}", operand(otherwise, rng)),
                None if spelling.flip_keyword_case && rng.percent(50) => {
                    format!(" {}", keyword("ELSE NULL", spelling, rng))
                }
                None => String::new(),
            };
            let case = keyword("CASE", spelling, rng);
            format!("{case} WHEN {condition} THEN {then}{otherwise} END")
        }
        Tree::Extract(inner) => {
            let extract = keyword("EXTRACT", spelling, rng);
            format!("{extract}(YEAR FROM {})", operand(inner, rng))
        }
        Tree::Substring(text, from, length) => {
            let (text, from, length) =
                (operand(text, rng), operand(from, rng), operand(length, rng));
            format!("SUBSTRING({text} FROM {from} FOR {length})")
        }
        Tree::Position(needle, haystack) => {
            let (needle, haystack) = (operand(needle, rng), operand(haystack, rng));
            format!("POSITION({needle} IN {haystack})")
        }
        Tree::Tuple(first, second) => {
            format!("({}, {})", operand(first, rng), operand(second, rng))
        }
        Tree::InQuery(subject, filter, negated, table) => {
            let subject = operand(subject, rng);
            let not = if *negated { "NOT " } else { "" };
            let not = keyword(not, spelling, rng);
            let query = subquery("a", filter.as_deref(), *table, spelling, rng);
            format!("{subject} {not}IN ({query})")
        }
        Tree::Exists(filter, negated, table) => {
            let not = if *negated { "NOT " } else { "" };
            let exists = keyword(&format!("{not}EXISTS"), spelling, rng);
            format!(
                "{exists} ({})",
                subquery("*", filter.as_deref(), *table, spelling, rng)
            )
        }
        Tree::Scalar(filter, table) => format!(
            "({})",
            subquery("b", filter.as_deref(), *table, spelling, rng)
        ),
    }
}

fn subquery(
    projection: &str,
    filter: Option<&Tree>,
    table: Table,
    spelling: Spelling,
    rng: &mut Rng,
) -> String {
    let flip_table_case = |rng: &mut Rng| spelling.flip_table_case && rng.percent(50);
    let name = if flip_table_case(rng) { "U" } else { "u" };
    let synonym = |rng: &mut Rng| spelling.inheritance_synonyms && rng.percent(50);
    // Where the dialect reads no inheritance markers, `ONLY` is a table name and keeps its case.
    let only = |rng: &mut Rng| {
        if spelling.inheritance_synonyms {
            keyword("ONLY", spelling, rng)
        } else {
            "ONLY".to_string()
        }
    };
    let name = match (table.only, synonym(rng)) {
        (true, true) => format!("{} ({name})", only(rng)),
        (true, false) => format!("{} {name}", only(rng)),
        (false, true) => format!("{name} *"),
        (false, false) => name.to_string(),
    };
    let alias = if table.aliased {
        let name = if flip_table_case(rng) { "V" } else { "v" };
        // `AS` before a table alias is optional, so writing it is a spelling.
        if spelling.flip_keyword_case && rng.percent(50) {
            format!(" {} {name}", keyword("AS", spelling, rng))
        } else {
            format!(" {name}")
        }
    } else {
        String::new()
    };
    let (select, from, filter_keyword) = (
        keyword("SELECT", spelling, rng),
        keyword("FROM", spelling, rng),
        keyword("WHERE", spelling, rng),
    );
    // A missing filter and `WHERE TRUE` keep the same rows where `TRUE` is reserved.
    let filter = match filter {
        Some(filter) => format!(" {filter_keyword} {}", spell(filter, spelling, rng)),
        None if spelling.true_filter_synonym && rng.percent(50) => {
            format!(" {filter_keyword} {}", keyword("TRUE", spelling, rng))
        }
        None => String::new(),
    };
    format!("{select} {projection} {from} {name}{alias}{filter}")
}

fn parse(dialect: &dyn Dialect, predicate: &str) -> Option<Expr> {
    let mut parser = Parser::new(dialect).try_with_sql(predicate).ok()?;
    let expr = parser.parse_expr().ok()?;
    matches!(parser.peek_token_ref().token, Token::EOF).then_some(expr)
}

fn dialects() -> [(&'static str, Box<dyn Dialect>, &'static str); 5] {
    [
        ("PostgreSQL", Box::new(PostgreSqlDialect {}), "\"Q\""),
        ("MySQL", Box::new(MySqlDialect {}), "`Q`"),
        ("SQLite", Box::new(SQLiteDialect {}), "\"Q\""),
        ("ANSI", Box::new(AnsiDialect {}), "\"Q\""),
        ("Generic", Box::new(GenericDialect {}), "\"Q\""),
    ]
}

fn canonicalize(
    canonicalizer: &Canonicalizer<'_>,
    predicate: &str,
) -> Result<String, CanonicalizeError> {
    canonicalizer.normalize_sql(&format!("SELECT * FROM t WHERE {predicate}"))
}

#[test]
fn canonical_text_means_what_the_input_means() {
    for (name, dialect, quoted) in dialects() {
        let dialect = dialect.as_ref();
        let canonicalizer = Canonicalizer::new(dialect);
        let folding = Folding::of(dialect);
        for seed in 0..SEEDS {
            let mut rng = Rng::new(seed);
            let predicate = spell(&tree(&mut rng, 3, quoted), Spelling::default(), &mut rng);
            let Some(input) = parse(dialect, &predicate) else {
                continue;
            };
            let Ok(canonical) = canonicalize(&canonicalizer, &predicate) else {
                continue;
            };
            let read_back = parse(dialect, &canonical)
                .unwrap_or_else(|| panic!("{name}: {canonical} from {predicate} does not parse"));
            assert_eq!(
                meaning(&read_back, folding),
                meaning(&input, folding),
                "{name}: {predicate} canonicalized to {canonical}, which means something else"
            );
        }
    }
}

#[test]
fn equivalent_spellings_agree() {
    let reference = Spelling {
        parenthesize_every_operand: true,
        ..Spelling::default()
    };
    for (name, dialect, quoted) in dialects() {
        let dialect = dialect.as_ref();
        let canonicalizer = Canonicalizer::new(dialect);
        let folding = Folding::of(dialect);
        let variant = Spelling {
            parenthesize_every_operand: true,
            flip_name_case: !dialect.is::<GenericDialect>(),
            flip_keyword_case: true,
            swap_symmetric_operands: true,
            swap_comparison_operands: Folding::of(dialect).collation_is_symmetric,
            double_colon_cast: dialect.is::<PostgreSqlDialect>(),
            flip_table_case: !dialect.is::<MySqlDialect>() && !dialect.is::<GenericDialect>(),
            true_filter_synonym: Folding::of(dialect).true_is_reserved,
            inheritance_synonyms: dialect.supports_table_inheritance_markers(),
            mirror_comparisons: Folding::of(dialect).collation_is_symmetric,
            swap_numeric_operands: Folding::of(dialect).plus_is_numeric,
        };
        for seed in 0..SEEDS {
            let mut rng = Rng::new(seed);
            let generated = tree(&mut rng, 3, quoted);
            let predicate = spell(&generated, Spelling::default(), &mut rng);
            let (Some(input), Some(intended)) = (
                parse(dialect, &predicate),
                parse(dialect, &spell(&generated, reference, &mut rng)),
            ) else {
                continue;
            };
            // Only a spelling the dialect groups the way the tree does is equivalent to the
            // fully parenthesized variant.
            if meaning(&input, folding) != meaning(&intended, folding) {
                continue;
            }
            let respelled = spell(&generated, variant, &mut rng);
            assert_eq!(
                canonicalize(&canonicalizer, &predicate),
                canonicalize(&canonicalizer, &respelled),
                "{name}: {predicate} and {respelled} are one predicate"
            );
        }
    }
}
