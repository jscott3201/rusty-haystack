"""Tests for SCRAM SHA-256 auth functions."""

import base64
import json
from pathlib import Path

import pytest
import rusty_haystack as rh


class TestGenerateNonce:
    def test_returns_string(self):
        nonce = rh.auth.generate_nonce()
        assert isinstance(nonce, str)
        assert len(nonce) > 0

    def test_unique(self):
        nonces = {rh.auth.generate_nonce() for _ in range(100)}
        assert len(nonces) == 100


class TestClientFirstMessage:
    def test_returns_tuple(self):
        nonce, client_first_b64 = rh.auth.client_first_message("admin")
        assert isinstance(nonce, str)
        assert isinstance(client_first_b64, str)
        assert len(nonce) > 0
        assert len(client_first_b64) > 0

    def test_contains_username(self):
        import base64
        nonce, client_first_b64 = rh.auth.client_first_message("testuser")
        decoded = base64.urlsafe_b64decode(client_first_b64 + "=" * (-len(client_first_b64) % 4)).decode()
        assert "testuser" in decoded


class TestDeriveCredentials:
    def test_returns_bytes_tuple(self):
        salt = b"\x00" * 16
        stored_key, server_key = rh.auth.derive_credentials("password", salt, 4096)
        assert isinstance(stored_key, bytes)
        assert isinstance(server_key, bytes)
        assert len(stored_key) == 32  # SHA-256
        assert len(server_key) == 32

    def test_deterministic(self):
        salt = b"\x01\x02\x03\x04" * 4
        a = rh.auth.derive_credentials("pass", salt, 4096)
        b = rh.auth.derive_credentials("pass", salt, 4096)
        assert a == b

    def test_different_passwords(self):
        salt = b"\x00" * 16
        a = rh.auth.derive_credentials("pass1", salt, 4096)
        b = rh.auth.derive_credentials("pass2", salt, 4096)
        assert a != b

    def test_different_salts(self):
        a = rh.auth.derive_credentials("pass", b"\x00" * 16, 4096)
        b = rh.auth.derive_credentials("pass", b"\x01" * 16, 4096)
        assert a != b


class TestExtractClientNonce:
    def test_roundtrip(self):
        original_nonce, client_first_b64 = rh.auth.client_first_message("user")
        extracted = rh.auth.extract_client_nonce(client_first_b64)
        assert extracted == original_nonce


class TestParseAuthHeader:
    def test_bearer(self):
        result = rh.auth.parse_auth_header("BEARER authToken=abc123")
        assert isinstance(result, dict)
        assert result.get("type") == "bearer"

    def test_hello(self):
        import base64
        username_b64 = base64.urlsafe_b64encode(b"admin").decode().rstrip("=")
        header = f"HELLO username={username_b64}"
        result = rh.auth.parse_auth_header(header)
        assert isinstance(result, dict)
        assert result.get("type") == "hello"


class TestFormatHelpers:
    def test_format_www_authenticate(self):
        result = rh.auth.format_www_authenticate("token123", "data456")
        assert isinstance(result, str)
        assert "token123" in result

    def test_format_auth_info(self):
        result = rh.auth.format_auth_info("authToken123", "data456")
        assert isinstance(result, str)
        assert "authToken123" in result


def test_independent_rfc7677_transcript_fixture():
    fixture = json.loads((Path(__file__).parents[2] / "haystack-core/tests/fixtures/auth-conformance.json").read_text())
    outer = fixture["derived_outer"]
    proof, verifier = rh.auth.client_final_message(
        fixture["published"]["password"], outer["client_first_data"], outer["server_first_data"]
    )
    assert proof == outer["client_final_data"]
    final = base64.urlsafe_b64decode(outer["server_final_data"] + "=" * (-len(outer["server_final_data"]) % 4)).decode()
    assert verifier == base64.b64decode(final.removeprefix("v="))


def test_optional_token_and_discovery_shape():
    assert rh.auth.format_www_authenticate() == "SCRAM hash=SHA-256"
    parsed = rh.auth.parse_auth_header("SCRAM data=bg")
    assert parsed == {"type": "scram", "handshake_token": None, "data": "bg"}
    assert rh.auth.parse_auth_header("HELLO username=am9zw6ksPeW3pQ") == {"type": "hello", "username": "josé,=工"}


@pytest.mark.parametrize("header", [
    "HELLO username=dXNlcg==", "HELLO username=dXNlcg, data=bg",
    "SCRAM data=_w", "SCRAM data=bg, data=bg", "BEARER authToken=secret, authToken=secret",
])
def test_malformed_headers_are_rejected_without_echo(header):
    with pytest.raises(Exception) as error:
        rh.auth.parse_auth_header(header)
    assert header not in str(error.value)
