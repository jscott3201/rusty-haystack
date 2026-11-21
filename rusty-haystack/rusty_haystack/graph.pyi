"""Public graph API."""

from typing import Any
from . import HDict, HGrid
from .ontology import DefNamespace

class DiffOp:
    """Type of graph change: Add, Update, or Remove."""
    Add: DiffOp
    Update: DiffOp
    Remove: DiffOp
    def __repr__(self) -> str: ...


class GraphDiff:
    """Record of a single change to the entity graph."""
    @property
    def version(self) -> int: ...
    @property
    def timestamp(self) -> int: ...
    @property
    def op(self) -> DiffOp: ...
    @property
    def ref_val(self) -> str: ...
    @property
    def old(self) -> HDict | None:
        """Entity state before the mutation. Populated for Remove only.

        An Update reports None here; read previous_tags instead.
        """
        ...
    @property
    def new(self) -> HDict | None:
        """Entity state after the mutation. Populated for Add only.

        An Update reports None here; read changed_tags instead.
        """
        ...
    @property
    def changed_tags(self) -> HDict | None:
        """For Update: tags that changed with their new values."""
        ...
    @property
    def previous_tags(self) -> HDict | None:
        """For Update: tags that changed with their previous values."""
        ...
    def __repr__(self) -> str: ...


class EntityGraph:
    """In-memory entity graph with bitmap tag indexing, filter queries, and change tracking.

    Examples:
        g = EntityGraph()
        g.add(HDict({"id": Ref("site-1"), "site": Marker(), "dis": "HQ"}))
        results = g.read("site")
    """
    def __init__(self) -> None: ...
    @staticmethod
    def with_namespace(ns: DefNamespace) -> EntityGraph:
        """Create a graph with an attached ontology namespace for validation.

        The namespace is shared, not consumed: ns stays fully usable and can be
        attached to further graphs. Calling ns.load_xeto() or ns.unload_lib()
        afterwards forks it, leaving graphs already built from it unchanged.
        """
        ...
    @staticmethod
    def from_grid(grid: HGrid, ns: DefNamespace | None = None) -> EntityGraph:
        """Bulk-load entities from an HGrid. Each row must have an 'id' Ref tag.

        The namespace, if given, is shared rather than consumed.
        """
        ...
    def add(self, entity: HDict) -> str:
        """Add an entity (must have an 'id' Ref tag). Returns the ref value string."""
        ...
    def add_grid(self, grid: HGrid) -> int:
        """Add multiple entities from an HGrid. Returns number added."""
        ...
    def get(self, ref_val: str) -> HDict | None:
        """Get an entity by ref value. Returns HDict or None."""
        ...
    def update(self, ref_val: str, changes: HDict) -> None:
        """Update an entity by merging changes into existing tags."""
        ...
    def remove(self, ref_val: str) -> HDict:
        """Remove an entity by ref value. Returns the removed HDict."""
        ...
    def read(self, filter_expr: str, limit: int = 0) -> HGrid:
        """Run a filter expression and return matching entities as a grid.

        Args:
            filter_expr: Haystack filter string (e.g., 'site', 'temp > 72').
            limit: Max results (0 for unlimited).
        """
        ...
    def all(self) -> list[HDict]:
        """Return all entities as a list of HDict."""
        ...
    def is_empty(self) -> bool:
        """Return True if the graph has no entities."""
        ...
    def validate(self) -> list[str]:
        """Validate all entities against the attached namespace."""
        ...
    def changes_since(self, version: int) -> list[GraphDiff]:
        """Return changelog entries since a given graph version.

        Raises RuntimeError if the subscriber has fallen behind (changelog gap).
        """
        ...
    def index_field(self, field: str) -> None:
        """Enable a B-tree value index on a tag for faster range queries."""
        ...
    def refs_from(self, ref_val: str, ref_type: str | None = None) -> list[str]:
        """Get ref values that the given entity points to."""
        ...
    def refs_to(self, ref_val: str, ref_type: str | None = None) -> list[str]:
        """Get ref values of entities pointing to the given entity."""
        ...
    def to_grid(self, filter_expr: str = "") -> HGrid:
        """Export matching entities to a grid. Empty filter exports all."""
        ...
    def ref_chain(self, ref_val: str, ref_tags: list[str]) -> list[HDict]:
        """Walk the ref chain from an entity following the given ref tags in order.

        Returns a list of HDict entities along the chain.
        """
        ...
    def site_for(self, ref_val: str) -> HDict | None:
        """Find the site entity for any entity by walking up the ref chain."""
        ...
    def children_of(self, ref_val: str) -> list[HDict]:
        """Get all direct children of an entity (entities whose xxxRef points to it)."""
        ...
    def equip_points(
        self, equip_ref: str, filter: str | None = None
    ) -> list[HDict]:
        """Get all points for an equip, optionally filtered.

        Raises GraphError on invalid filter expression.
        """
        ...
    def hierarchy_tree(
        self, root: str, max_depth: int = 10
    ) -> dict[str, Any] | None:
        """Build a hierarchy tree from a root entity.

        Returns a nested dict: {"entity": HDict, "depth": int, "children": [...]},
        or None if the root is not found.
        """
        ...
    def classify(self, ref_val: str) -> str | None:
        """Classify an entity by its most specific type tag."""
        ...
    @property
    def version(self) -> int:
        """Current graph version (increments on every mutation)."""
        ...
    def __len__(self) -> int: ...
    def __contains__(self, ref_val: str, /) -> bool: ...
    def __repr__(self) -> str: ...


class SharedGraph:
    """Thread-safe shared entity graph backed by Arc<RwLock<EntityGraph>>.

    Safe for concurrent reads and serialized writes.

    Note: the constructor consumes its EntityGraph argument. An entity graph is
    mutable state and cannot be shared by copying, so it genuinely moves. The
    source EntityGraph is left poisoned: every later call on it raises
    RuntimeError rather than silently operating on an empty graph the
    SharedGraph cannot see. A DefNamespace attached to it is shared, not
    consumed, and the SharedGraph inherits it.
    """
    def __init__(self, graph: EntityGraph | None = None) -> None: ...
    @staticmethod
    def from_grid(grid: HGrid, ns: DefNamespace | None = None) -> SharedGraph:
        """Create a SharedGraph from an HGrid.

        The namespace, if given, is shared rather than consumed.
        """
        ...
    def add(self, entity: HDict) -> str:
        """Add an entity. Returns the ref value string."""
        ...
    def get(self, ref_val: str) -> HDict | None:
        """Get an entity by ref value. Returns HDict or None."""
        ...
    def update(self, ref_val: str, changes: HDict) -> None:
        """Update an entity by merging changes."""
        ...
    def remove(self, ref_val: str) -> HDict:
        """Remove an entity. Returns the removed HDict."""
        ...
    def read(self, filter_expr: str, limit: int = 0) -> HGrid:
        """Query entities matching a Haystack filter."""
        ...
    def all(self) -> list[HDict]:
        """Return all entities as a list of HDict."""
        ...
    def is_empty(self) -> bool:
        """Return True if the graph has no entities."""
        ...
    def contains(self, ref_val: str) -> bool:
        """Return True if an entity with the given ref exists."""
        ...
    def refs_from(self, ref_val: str, ref_type: str | None = None) -> list[str]:
        """Get ref values that the given entity points to."""
        ...
    def refs_to(self, ref_val: str, ref_type: str | None = None) -> list[str]:
        """Get ref values of entities pointing to the given entity."""
        ...
    def changes_since(self, version: int) -> list[GraphDiff]:
        """Return changelog entries since a given graph version.

        Raises RuntimeError if the subscriber has fallen behind (changelog gap).
        """
        ...
    def validate(self) -> list[str]:
        """Validate all entities against the attached namespace."""
        ...
    @property
    def subscriber_count(self) -> int:
        """Number of active broadcast subscribers."""
        ...
    def ref_chain(self, ref_val: str, ref_tags: list[str]) -> list[HDict]:
        """Walk the ref chain from an entity following the given ref tags in order."""
        ...
    def site_for(self, ref_val: str) -> HDict | None:
        """Find the site entity for any entity by walking up the ref chain."""
        ...
    def children_of(self, ref_val: str) -> list[HDict]:
        """Get all direct children of an entity."""
        ...
    def equip_points(
        self, equip_ref: str, filter: str | None = None
    ) -> list[HDict]:
        """Get all points for an equip, optionally filtered.

        Raises GraphError on invalid filter expression.
        """
        ...
    def hierarchy_tree(
        self, root: str, max_depth: int = 10
    ) -> dict[str, Any] | None:
        """Build a hierarchy tree from a root entity.

        Returns a nested dict or None if the root is not found.
        """
        ...
    def classify(self, ref_val: str) -> str | None:
        """Classify an entity by its most specific type tag."""
        ...
    @property
    def version(self) -> int:
        """Current graph version."""
        ...
    def __len__(self) -> int: ...
    def __repr__(self) -> str: ...
