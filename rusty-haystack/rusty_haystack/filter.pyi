"""Public filter API."""

from . import HDict, _Value, _InputValue
from .ontology import DefNamespace

class CmpOp:
    """Comparison operator for filter expressions."""
    Eq: CmpOp
    Ne: CmpOp
    Lt: CmpOp
    Le: CmpOp
    Gt: CmpOp
    Ge: CmpOp
    def __repr__(self) -> str: ...


class Path:
    """Haystack filter path for tag traversal (e.g., 'equipRef->siteRef->area')."""
    def __init__(self, segments: list[str]) -> None: ...
    @staticmethod
    def single(name: str) -> Path:
        """Create a single-segment path."""
        ...
    @property
    def segments(self) -> list[str]:
        """Path segments."""
        ...
    def is_single(self) -> bool:
        """True if this path has only one segment."""
        ...
    def first(self) -> str:
        """First segment name."""
        ...
    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...
    def __len__(self) -> int: ...


class Filter:
    """Haystack filter expression with builder pattern and AST inspection.

    Filters can be constructed by parsing or using the builder API.
    Supports Python & (and) and | (or) operators.

    Examples:
        f = Filter.parse("site and area > 1000")
        f = Filter.has("site") & Filter.eq("area", Number(1000))
        f.matches(entity)
    """
    @staticmethod
    def parse(expr: str) -> Filter:
        """Parse a Haystack filter expression string."""
        ...
    @staticmethod
    def has(tag: str) -> Filter:
        """Create a 'has' filter: tag is present."""
        ...
    @staticmethod
    def missing(tag: str) -> Filter:
        """Create a 'missing' filter: tag is absent."""
        ...
    @staticmethod
    def cmp(path: Path, op: CmpOp, val: _InputValue) -> Filter:
        """Create a comparison filter with a Path, operator, and value."""
        ...
    @staticmethod
    def eq(tag: str, val: _InputValue) -> Filter:
        """Create an equals comparison: tag == val."""
        ...
    def and_(self, other: Filter) -> Filter:
        """Combine with AND."""
        ...
    def or_(self, other: Filter) -> Filter:
        """Combine with OR."""
        ...
    def matches(self, entity: HDict, namespace: DefNamespace | None = None) -> bool:
        """Evaluate this filter against an entity dict.

        `namespace` is required for spec-match terms such as `ph::Point`.
        Evaluating one without a namespace raises FilterError rather than
        reporting a non-match.
        """
        ...
    @property
    def node_type(self) -> str:
        """Node type: 'has', 'missing', 'cmp', 'and', 'or', 'specMatch'."""
        ...
    @property
    def path(self) -> Path | None:
        """For Has/Missing/Cmp nodes, the tag path. None for And/Or."""
        ...
    @property
    def op(self) -> CmpOp | None:
        """For Cmp nodes, the comparison operator. None for others."""
        ...
    def val(self) -> _Value:
        """For Cmp nodes, the comparison value. None for others."""
        ...
    def children(self) -> tuple[Filter, Filter] | None:
        """For And/Or nodes, return (left, right). None for others."""
        ...
    def __and__(self, other: Filter, /) -> Filter: ...
    def __or__(self, other: Filter, /) -> Filter: ...
    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...
    def __eq__(self, other: object, /) -> bool: ...
    def __ne__(self, other: object, /) -> bool: ...


def parse_filter(expr: str) -> str:
    """Parse a filter expression and return its AST as a debug string."""
    ...


def matches_filter(
    filter_expr: str, entity: HDict, namespace: DefNamespace | None = None
) -> bool:
    """Evaluate a filter expression against an entity dict.

    `namespace` is required for spec-match terms such as `ph::Point`.
    Raises FilterError if the expression is invalid, or if it contains a
    spec-match term and no namespace was given.
    """
    ...
