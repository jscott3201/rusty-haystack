"""Public auth API."""


def derive_credentials(
    password: str, salt: bytes, iterations: int
) -> tuple[bytes, bytes]:
    """Derive SCRAM SHA-256 credentials. Returns (stored_key, server_key)."""
    ...


def generate_nonce() -> str:
    """Generate a random SCRAM nonce with 144 bits of entropy."""
    ...


def client_first_message(username: str) -> tuple[str, str]:
    """Return (client_nonce, client_first_data), with unpadded base64url data."""
    ...


def client_final_message(
    password: str, client_first_data: str, server_first_data: str
) -> tuple[str, bytes]:
    """Return (client_final_data, expected_server_signature) using exact transcripts."""
    ...


def extract_client_nonce(client_first_b64: str) -> str:
    """Extract the nonce from base64url client-first data or raise AuthError."""
    ...


def parse_auth_header(header: str) -> dict[str, str | None]:
    """Parse HELLO, SCRAM, or BEARER credentials; SCRAM token is optional."""
    ...


def format_www_authenticate(
    handshake_token: str | None = None, data: str | None = None
) -> str:
    """Format a SHA-256 discovery or server-first challenge."""
    ...


def format_auth_info(auth_token: str, data_b64: str) -> str:
    """Format Authentication-Info with bearer token, SHA-256 hash, and data."""
    ...
