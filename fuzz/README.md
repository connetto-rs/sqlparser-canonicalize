# Fuzzing

Two targets, both asserting that accepted canonical text reads back as itself. `normalize_sql` takes one selector byte, choosing the dialect and whether the input is a whole statement or a bare predicate. `normalize_where_clause` takes two, so the dialect that parses the expression can differ from the one that verifies the canonical text.

Seeds are tracked here, corpora are not. Copy the seeds in before the first run:

```sh
mkdir -p corpus/normalize_sql && cp seeds/normalize_sql/* corpus/normalize_sql/
cargo +nightly fuzz run normalize_sql -- -max_len=10000 -timeout=1 -dict=fuzz/sql.dict
```

For a campaign, add `-fork=8 -ignore_crashes=1` so one finding does not stop the run, then replay each artifact individually. `-max_len` must exceed 8192 for the input length guard to be reachable.
