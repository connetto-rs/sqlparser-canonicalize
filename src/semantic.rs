use sqlparser::ast::{DataType, Expr, Ident};

use crate::CanonicalizeError;

/// Explicit cast targets proved equivalent to a comparison's selected operand conversions.
#[derive(Clone, Copy, Debug)]
pub struct ComparisonCoercions<'a> {
    /// The left operand's cast target, including its proved qualification and typmod.
    pub left: Option<&'a DataType>,
    /// The right operand's cast target, including its proved qualification and typmod.
    pub right: Option<&'a DataType>,
}

/// Authorizes semantic rendering operations on exact occurrences in one immutable expression.
///
/// Implementations own the semantic proof, including resolved operation and conversion identities,
/// effective collation, and the output scope in which emitted references will execute.
/// Occurrences are identified by their original location, never by structural or textual equality.
pub trait SemanticContext {
    /// Returns the original expression whose occurrences this context accepts.
    fn expression(&self) -> &Expr;

    /// Returns a column spelling proved to resolve to the same range-table occurrence in the output scope.
    ///
    /// `None` grants no rewrite, and returned identifiers must form a nonempty reference.
    ///
    /// # Errors
    /// Returns an error for a foreign occurrence or unavailable required proof.
    fn column_reference(&self, occurrence: &Expr) -> Result<Option<&[Ident]>, CanonicalizeError>;

    /// Returns explicit cast targets proved to preserve a resolved binary comparison.
    ///
    /// Each target must preserve the selected conversion's typmod, implicit-versus-explicit behavior,
    /// and effective collation when emitted as `CAST`, including lookup of the supplied type spelling.
    /// `None` grants no authorization, while two absent targets prove no materialization is needed.
    /// Neither result authorizes reordering or removal of explicit casts.
    ///
    /// # Errors
    /// Returns an error for a foreign occurrence or unavailable required proof.
    fn comparison_coercions(
        &self,
        occurrence: &Expr,
    ) -> Result<Option<ComparisonCoercions<'_>>, CanonicalizeError>;
}
