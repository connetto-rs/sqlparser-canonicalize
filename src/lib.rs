#![no_std]
#![doc = include_str!("../README.md")]

extern crate alloc;

use alloc::string::String;

use thiserror::Error;

mod canonicalize;
mod semantic;

pub use canonicalize::{Canonicalizer, hash_canonical};
pub use semantic::{
    ArrayCoercions, ComparisonCoercions, Conversion, MembershipCoercions, MembershipElement,
    SemanticContext,
};

/// Why a predicate could not be turned into canonical text.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[non_exhaustive]
pub enum CanonicalizeError {
    /// The SQL did not parse. The message comes from the parser.
    #[error("SQL parse error: {0}")]
    Parse(String),
    /// The input was longer than the crate will accept. Shortening it may succeed.
    #[error("SQL input is longer than {limit} bytes")]
    InputTooLong { limit: usize },
    /// The expression nested deeper than the crate will walk. Simplifying it may succeed.
    #[error("Expression nests deeper than {limit} levels")]
    TooDeep { limit: usize },
    /// The predicate has no canonical spelling the parser can read back unchanged, so
    /// hashing it would risk giving two different predicates one key.
    #[error("Canonical text would not read back as itself: {0}")]
    NotRoundTrippable(String),
    /// Syntax this crate does not canonicalize.
    #[error("Unsupported SQL: {0}")]
    Unsupported(String),
}
