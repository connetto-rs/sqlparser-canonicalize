use core::cell::Cell;
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;

use sqlparser::dialect::PostgreSqlDialect;
use sqlparser_canonicalize::Canonicalizer;

// Thread-local, because the test harness keeps allocating on its own thread while the test runs.
thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

// SAFETY: Every allocation and deallocation is forwarded unchanged to `System`.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `GlobalAlloc` callers provide a valid layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && ENABLED.get() {
            ALLOCATIONS.set(ALLOCATIONS.get() + 1);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: `GlobalAlloc` callers return the original pointer and layout pair.
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn measure(sql: &str) -> usize {
    ALLOCATIONS.set(0);
    ENABLED.set(true);
    let normalized = black_box(
        Canonicalizer::new(black_box(&PostgreSqlDialect {})).normalize_sql(black_box(sql)),
    )
    .unwrap();
    ENABLED.set(false);
    black_box(normalized);
    ALLOCATIONS.get()
}

#[test]
fn fixed_corpus_allocation_count_does_not_grow() {
    let sql = "SELECT * FROM t WHERE (a = 1 AND b = 2) OR (c = 3 AND d = 4) AND e IN (5, 6, 7)";
    let _ = measure(sql);
    let counts = std::array::from_fn::<_, 16, _>(|_| measure(sql));
    // Two passes over the predicate, the original statement parse and the expression
    // re-read that proves the canonical text reads as itself.
    assert_eq!(counts, [363; 16]);
}
