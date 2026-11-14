use sqlparser::ast::{DataType, Expr, Ident};

use crate::CanonicalizeError;

/// A proved conversion of one operand, as the server spells the converted operand.
///
/// The conversion applies under the operand's `COLLATE` clauses, as PostgreSQL applies it.
#[derive(Clone, Copy, Debug)]
pub enum Conversion<'a> {
    /// Spelled `CAST(operand AS target)`, with the target's proved qualification and typmod.
    ///
    /// A collated operand keeps its collation, so `'a' COLLATE "C"` cast to `TEXT` is spelled
    /// `CAST('a' AS TEXT) COLLATE "C"`.
    Cast(&'a DataType),
    /// A literal operand the server converts when it reads the expression, spelled as the
    /// constant it becomes, such as `1` for `'1'` compared with an `INTEGER`.
    ///
    /// The constant must be a literal or a cast of one, and only a literal operand takes it. A
    /// literal under `COLLATE` takes it only when the constant's type is not collatable, because
    /// the constant replaces the collated literal and PostgreSQL drops such a collation.
    Constant(&'a Expr),
}

/// Proved conversions of a comparison's operands.
#[derive(Clone, Copy, Debug)]
pub struct ComparisonCoercions<'a> {
    /// The left operand's conversion.
    pub left: Option<Conversion<'a>>,
    /// The right operand's conversion.
    pub right: Option<Conversion<'a>>,
}

/// Proved conversions of the array comparison an `IN` list resolves to.
#[derive(Clone, Copy, Debug)]
pub struct ArrayCoercions<'a> {
    /// The tested operand's conversion.
    pub left: Option<Conversion<'a>>,
    /// The cast target of the array built from the elements, such as `TEXT[]`.
    pub array: Option<&'a DataType>,
}

/// How the server compares one element of an `IN` list with the tested operand.
#[derive(Clone, Copy, Debug)]
pub enum MembershipElement<'a> {
    /// An element of the array the tested operand is compared with, with its conversion.
    Array(Option<Conversion<'a>>),
    /// An element compared on its own, as the comparison's right operand.
    Comparison(ComparisonCoercions<'a>),
}

/// Proved resolution of an `IN` list into comparisons, as PostgreSQL resolves it.
///
/// `x IN (...)` becomes `x = ANY(ARRAY[...])` over the `Array` elements, followed by one
/// `x = element` per `Comparison` element in list order, joined left to right by `OR`.
/// `NOT IN` takes `!=`, `ALL` and `AND` instead.
#[derive(Clone, Copy, Debug)]
pub struct MembershipCoercions<'a> {
    /// The array comparison, present exactly when some element is an `Array` element.
    pub array: Option<ArrayCoercions<'a>>,
    /// One resolution per list element, in list order.
    pub elements: &'a [MembershipElement<'a>],
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

    /// Returns conversions proved to preserve a resolved binary comparison.
    ///
    /// Each conversion must preserve the selected conversion's typmod, implicit-versus-explicit
    /// behavior, and effective collation as spelled, including lookup of a supplied type spelling.
    /// `None` grants no authorization, while two absent conversions prove none is needed.
    /// Neither result authorizes reordering or removal of explicit casts.
    ///
    /// # Errors
    /// Returns an error for a foreign occurrence or unavailable required proof.
    fn comparison_coercions(
        &self,
        occurrence: &Expr,
    ) -> Result<Option<ComparisonCoercions<'_>>, CanonicalizeError>;

    /// Returns the proved resolution of an `IN` list occurrence into comparisons.
    ///
    /// Every comparison and conversion is bound by the same proof obligations as
    /// [`comparison_coercions`](Self::comparison_coercions). `None` keeps the `IN` list as written.
    ///
    /// # Errors
    /// Returns an error for a foreign occurrence or unavailable required proof.
    fn membership_coercions(
        &self,
        occurrence: &Expr,
    ) -> Result<Option<MembershipCoercions<'_>>, CanonicalizeError>;
}
