# sqlparser-canonicalize

[![CI](https://github.com/connetto-rs/sqlparser-canonicalize/actions/workflows/ci.yml/badge.svg)](https://github.com/connetto-rs/sqlparser-canonicalize/actions/workflows/ci.yml)
[![Coverage](https://codecov.io/gh/connetto-rs/sqlparser-canonicalize/branch/main/graph/badge.svg)](https://codecov.io/gh/connetto-rs/sqlparser-canonicalize)
[![Crates.io](https://img.shields.io/crates/v/sqlparser-canonicalize.svg)](https://crates.io/crates/sqlparser-canonicalize)
[![Docs](https://docs.rs/sqlparser-canonicalize/badge.svg)](https://docs.rs/sqlparser-canonicalize)
[![License](https://img.shields.io/crates/l/sqlparser-canonicalize.svg)](https://github.com/connetto-rs/sqlparser-canonicalize/blob/main/LICENSE)

`sqlparser-canonicalize` produces canonical predicate text and stable hashes from `sqlparser` syntax trees. Equivalent predicate spellings produce identical bytes for durable deduplication.

```rust
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser_canonicalize::{Canonicalizer, hash_canonical};

let canonicalizer = Canonicalizer::new(&PostgreSqlDialect {});

let normalized = canonicalizer.normalize_sql("SELECT * FROM orders WHERE status = 'paid'")?;
assert_eq!(normalized, "('paid' = status)");

// The same predicate written the other way round produces the same bytes, and so the same key.
let commuted = canonicalizer.normalize_sql("SELECT * FROM orders WHERE 'paid' = status")?;
assert_eq!(hash_canonical(&commuted), hash_canonical(&normalized));
# Ok::<(), sqlparser_canonicalize::CanonicalizeError>(())
```

Canonical text, and so every hash, is stable within a minor version. A new minor version may spell some predicates differently, so keys stored under one minor version are rebuilt after upgrading to the next. A predicate the crate cannot spell in one way that reads back as the same predicate, including a subquery carrying table hints or index directives, is refused, never keyed.

The dialect belongs to the canonicalizer because it decides the answer: whether `"Status"` and `Status` are one column or two is a PostgreSQL question, not a SQL one.

Normalization is O(n log n) for `AND` and `OR` chains because operands are sorted, and O(n) for other syntax tree shapes.

The crate works without the standard library if you turn off default features, at the cost of the parser's stack protection, so that configuration can be crashed by deeply nested input and should only take SQL you trust.
