"""Public server API."""

from asyncio import Future
from types import TracebackType
from typing import Any, Coroutine, Literal
from . import HGrid
from .graph import SharedGraph
from .ontology import DefNamespace

class ApplicationBuilder:
    """Frozen scoped in-memory configuration; start consumes it once.

    A supplied graph and the shared bridge runtime are borrowed. The listener
    is owned. host accepts numeric IP addresses only. auth is consumed at start,
    after validating the running loop and configuration. Without auth, HTTP
    entity data is denied while trusted embedded reads remain available.
    """
    def __init__(self, graph: SharedGraph | None = None, *, host: str = "127.0.0.1", port: int = 0, auth: AuthManager | None = None) -> None: ...
    def start(self) -> ApplicationOwner:
        """Require a running asyncio loop and return the owner immediately."""
        ...

class ApplicationTermination:
    """Actual cleanup receipt, preserving an earlier native shutdown failure."""
    @property
    def drain_expired(self) -> bool | None: ...
    @property
    def close_error(self) -> str | None: ...
    @property
    def cleanup_errors(self) -> tuple[str, ...]: ...

class ApplicationHandle:
    """Borrowed view with no shutdown authority; observes its owner's lifecycle."""
    @property
    def state(self) -> Literal["starting", "running", "closing", "closed", "failed"]: ...
    @property
    def closed(self) -> bool:
        """Actual termination, including failed cleanup; Failed alone is insufficient."""
        ...
    def ready(self, *, deadline: float | None = None) -> Future[tuple[str, int]]: ...
    def read(self, filter: str, limit: int = 100, *, cursor: str | None = None, deadline: float | None = None) -> Future[HGrid]: ...
    def terminated(self, *, deadline: float | None = None) -> Future[ApplicationTermination]: ...

class ApplicationOwner:
    """Sole owner. Entry is coroutine-compatible; ordinary operations are Futures."""
    @property
    def state(self) -> Literal["starting", "running", "closing", "closed", "failed"]: ...
    @property
    def closed(self) -> bool: ...
    def handle(self) -> ApplicationHandle: ...
    def ready(self, *, deadline: float | None = None) -> Future[tuple[str, int]]: ...
    def read(self, filter: str, limit: int = 100, *, cursor: str | None = None, deadline: float | None = None) -> Future[HGrid]: ...
    def close(self, *, deadline: float | None = None) -> Future[None]: ...
    def terminated(self, *, deadline: float | None = None) -> Future[ApplicationTermination]: ...
    def __aenter__(self) -> Coroutine[Any, Any, ApplicationOwner]: ...
    def __aexit__(self, exc_type: type[BaseException] | None, exc_value: BaseException | None, traceback: TracebackType | None) -> Future[None]: ...

class AuthManager:
    """SCRAM SHA-256 authentication manager for the server.

    Passing one to HaystackServer.with_auth consumes it. Every method on a
    consumed AuthManager raises RuntimeError, except repr(), which reports
    "AuthManager(consumed)".
    """
    @staticmethod
    def empty() -> AuthManager:
        """Create a disabled (no-auth) manager."""
        ...
    @staticmethod
    def from_toml(path: str) -> AuthManager:
        """Load auth configuration from a TOML file path."""
        ...
    @staticmethod
    def from_toml_str(content: str) -> AuthManager:
        """Load auth configuration from a TOML string."""
        ...
    def is_enabled(self) -> bool:
        """Whether authentication is enabled (has users configured)."""
        ...
    def __repr__(self) -> str: ...


class HisStore:
    """In-memory history storage for time-series point data."""
    def __init__(self) -> None: ...
    def len(self, id: str) -> int:
        """Number of history records for a given entity ID."""
        ...
    def __repr__(self) -> str: ...


class HaystackServer:
    """Embedded Haystack HTTP API server with builder-pattern configuration.

    Note: with_auth consumes its argument. The source AuthManager is left
    poisoned: every later call on it raises RuntimeError. with_namespace does
    not consume — it copies.

    Examples:
        server = HaystackServer(SharedGraph())
        server.with_auth(auth)
        server.port(8080)
        server.run()  # blocks
    """
    def __init__(self, graph: SharedGraph) -> None: ...
    def with_namespace(self, ns: DefNamespace) -> None:
        """Set the ontology namespace for validation and spec management.

        The namespace is copied into the server, so ns stays usable. The server
        keeps its own mutable copy because the lib load/unload endpoints mutate
        it; later changes on either side do not affect the other.
        """
        ...
    def with_auth(self, auth: AuthManager) -> None:
        """Set the auth manager, consuming it.

        `auth` is unusable afterwards and every later call on it raises
        RuntimeError. An AuthManager holds live SCRAM state — in-flight
        handshakes, issued tokens, and the server secret used to derive
        anti-enumeration challenges — so it moves rather than being shared or
        copied. Configure it fully before calling this.

        Raises:
            RuntimeError: if `auth` was already given to a server.
        """
        ...
    def port(self, port: int) -> None:
        """Set the HTTP listen port (default 8080)."""
        ...
    def host(self, host: str) -> None:
        """Set the HTTP listen host (default '0.0.0.0')."""
        ...
    def run(self) -> None:
        """Start the server, blocking the calling thread. Releases the GIL."""
        ...
    def run_background(self) -> None:
        """Start the server in a background thread. Returns immediately."""
        ...
    def bg_error(self) -> str | None:
        """Retrieve the background server error message, if any."""
        ...
    def __repr__(self) -> str: ...
