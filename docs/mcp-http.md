# Read-only MCP over local Streamable HTTP

The optional `http` feature of `rusty-haystack-mcp` serves the existing
`haystack_query` and `haystack_read` tools through official `rmcp = 3.5.1` and MCP
`2026-07-28`. The CLI selects it with `mcp-http`. HTTP-only builds do not enable
stdio, PostgreSQL, Python bindings, tasks, or new read/write capabilities. Select
`mcp,mcp-http` to install both independent CLI commands.
An explicitly selected history service adds [`haystack_history`](mcp-history.md)
under the same authenticated transport and each subject's History permission.
Explicit [H4 semantic resources](mcp-semantic-resources.md) use the same transport
with separate per-subject library and definition grants and a fixed H4 content
profile beside either entity-tool output profile.

This is a **numeric loopback HTTP/1 deployment profile for a trusted local
embedding**. It uses one POST per connection and bounded JSON responses. Clients
must accept both JSON and SSE as required by MCP, but these tools produce JSON;
there are no SSE event streams, legacy initialization, sessions, event stores,
replay, GET streams or DELETE sessions. Production TLS, reverse proxies, public
listeners, external identity-provider setup and agent-host/model integration are
outside this qualification.

## Configuration and launch

```sh
cargo +1.99.1 install --locked --path haystack-cli --features mcp-http
haystack mcp http --config /absolute/path/mcp-http.toml
```

An explicit version 1 TOML config supplies the dataset, numeric loopback listener,
trusted issuer and public keys, required scope, and each admitted subject's read
scope:

```toml
version = 1
dataset_file = "entities.trio"
dataset_format = "trio"
listen = "127.0.0.1:8765"
issuer = "https://issuer.example.test"
required_scope = "haystack:read"
request_timeout_ms = 10000
max_response_bytes = 1048576
max_connections = 32
max_in_flight = 16
allowed_origins = []
revoked_jti = []

[[keys]]
kid = "local-issuer-key-1"
algorithm = "RS256"
public_key_der_base64 = "<BASE64_OF_PKCS1_RSA_PUBLIC_KEY_DER>"

[[subjects]]
subject = "building-reader"
entity_ids = ["site-a", "ahu-a"]
hidden_tags = ["privateNote"]
```

Replace the key placeholder with a trusted RSA public key (2048–8192 bits), in
PKCS#1 DER encoding, then standard Base64. For example, a trusted key owner can
export the public portion with `openssl rsa -in KEY.pem -RSAPublicKey_out -outform
DER | openssl base64 -A`. The server never reads private keys. It rejects any
configured algorithm other than RS256. Up to 16 named keys and 512 distinct
subjects are supported. Each subject has 1–512 entity IDs and at most 512 hidden
tags, with individual subject/ID/tag strings bounded to 256 bytes.

The protected resource and required token audience are exactly
`http://<bound numeric address>/mcp`, such as `http://127.0.0.1:8765/mcp` or
`http://[::1]:8765/mcp`. Port zero is supported for local fixture/embedding use;
the actual resource is reported on stderr as `MCP HTTP ready: ...` after the
application is ready and adapter configuration has passed. The issuer must mint
a token for that actual resource. Host, Forwarded, tokens and request metadata
never select the resource identity. Stdin is unused; stdout stays empty.

The shared [stdio configuration rules](mcp-stdio.md#standalone-launch) apply to
dataset formats, path resolution, the 64 KiB config and 4 MiB/10,000-row dataset
bounds, conservative dataset depth, regular-file checks and sanitized launch
errors. Unknown fields fail closed. No ambient credential or dataset lookup is
performed. Subject scopes are immutable for this CLI lifetime; reload requires a
new process. Hidden tags and denied references use the shared read policy.

Output defaults to `haystack-h4-json-v1`. To select the
[rich-value profile](mcp-rich-values.md), set the top-level
`output_profile = "haystack-jeto-v1"`; any `nominal_specs` allowlist belongs inside
the appropriate `[[subjects]]` table. Every rich tool call must acknowledge the
profile. The installed HTTP fixture validates both profiles with independent
schemas and a decoder for the selected rich values.

Optional history uses top-level `history_file = "history.json"`, containing a
bounded trusted `typed-payload-v1` sample Grid, and `history_read = true` inside
each authorized `[[subjects]]` table. Ordinary entity permission does not grant
History. The selected history default/minimum response budget is 2 MiB for both
profiles; entity-only 8 KiB H4/16 KiB Jeto floors remain unchanged. See the
[history configuration and result contract](mcp-history.md).

## Token and request authority

The offline verifier selects a configured key by bounded `kid` and verifies an
RS256 signature using the existing `ring 0.17.14` implementation. It accepts only
signed JWT access tokens with `typ` equal to `at+jwt` or `application/at+jwt` and
required RFC 9068 claims `iss`, `sub`, `aud`, `exp`, `iat`, `client_id` and `jti`.
Issuer and resource audience must match the configured values; `aud` may be a
string or a bounded string array. Dates are nonnegative integer seconds, expiry
must be in the future, issued-at cannot be in the future or at/after expiry, and
an optional not-before cannot be in the future or at/after expiry. This profile
uses no clock-skew allowance. Keep the local clock synchronized.

Tokens are limited to 8 KiB and JSON depth eight. Duplicate JSON members, unknown
protected header members, unsupported critical headers, `jku`/`x5u`, unsigned or
encrypted token forms, algorithm confusion and unconfigured keys fail closed.
Token content never triggers network access, JWKS fetches, introspection or cache
fills. A present scope claim must be a nonempty string of RFC 6749 scope tokens,
separated by single ASCII spaces. Malformed scope claims invalidate the token;
an absent or valid but insufficient scope produces 403. A valid token must contain
the configured read scope and an admitted subject. Only that verified identity and selected permission reach
`Principal::Authenticated`; tool arguments and metadata cannot supply authority.
Every page still takes a current shared policy snapshot and validates its cursor.
Cursors remain bound to the principal, query, projection, page size and scope.

Known revoked token IDs are held in a local set of at most 1024 entries. The CLI
loads `revoked_jti` at startup; trusted embeddings can call `JwtVerifier::revoke`
to add an ID, with every later request rechecking the set. Revocation does not
retroactively cancel a previously admitted request. **Offline verification does
not learn issuer-side revocation**; the owner must supply an update or restart
with updated configuration. This adapter makes no remote revocation guarantee.

The metadata endpoint is
`GET /.well-known/oauth-protected-resource/mcp`. Its resource, issuer and supported
scope come only from trusted configuration. Missing credentials produce 401 with
a Bearer `resource_metadata`/`scope` challenge and no token error. Invalid,
expired or locally revoked tokens produce 401 `invalid_token`; valid credentials
without admitted subject/scope produce 403 `insufficient_scope`. Malformed or
duplicate Authorization and token-bearing query strings are rejected with 400
`invalid_request`. Only the Authorization header presents a token.

A present Host must exactly equal the bound numeric host and port. Missing or
unapproved Origin is handled deliberately: missing is allowed; present Origin
must exactly match `allowed_origins`, and an empty list rejects all present
origins. `null`, duplicate or malformed origins cannot bypass this check. No
proxy is trusted: Forwarded and X-Forwarded-* headers and absolute-form request
URIs are rejected. The same authority policy protects metadata.

Each request has matching `MCP-Protocol-Version`/`Mcp-Method` headers and
`params._meta` containing `io.modelcontextprotocol/protocolVersion` and
`io.modelcontextprotocol/clientCapabilities`. Tool calls also require matching
`Mcp-Name`, including the specified Base64 representation when used. Selected
`resources/read` requests require that header to equal `params.uri`. Duplicate
singleton headers, missing or mismatched metadata, invalid JSON-RPC IDs, batches,
client result/error envelopes, unavailable methods and malformed selected
requests are rejected before application-work admission. Unsupported methods use
404/-32601; unsupported versions use 400/-32022; header mismatch uses
400/-32020; missing/malformed body metadata uses 400/-32602. An unidentified error
omits its ID. Unknown tools are rejected before the SDK's schema cache lookup.

Tool schemas, H4 result envelopes, strict rich-value projection, masking,
continuation and application errors are the same as the
[stdio tool profile](mcp-stdio.md#tools-and-result-profile).

## Bounds and lifecycle

Defaults are 32 accepted connections, 16 admitted MCP requests, 16 KiB HTTP parser
buffer, 64 headers, 64 KiB body, JSON depth 32, 128 bytes per string request ID,
1 MiB full response and a 10-second absolute accept-to-delivery deadline. The CLI
allows 1–128 connections/in-flight requests, 8 KiB–16 MiB responses for H4 or
16 KiB–16 MiB for Jeto, and 10–60,000 ms deadlines. The profile-specific response
minimum allows both discovery schemas and a maximally escaped 128-byte request
ID; smaller configurations fail before serving. Embeddings enforce the same
response minimum and can also configure the bounded parser/body/
depth limits. The adapter requests 16 KiB kernel send/receive buffers (the OS may
round or double them) and caps each write to 16 KiB. Excess connections close;
excess valid requests receive 503. Input collection and response materialization
are capped before unbounded growth; each accepted connection owns at most one
request, and the SDK's fixed internal channel has capacity 16. Shared read
budgets apply independently, including the default five-second work deadline and
conservative one-MiB application output reservation.

No partial-header/body trickle, held worker or non-reading peer can extend the
absolute transport deadline. Disconnect, deadline and explicit/application close
suppress the connection and cancel its request. Every request retains its
transport permit and native work registration through both actual joined-worker
exit and response completion/suppression. The shared read is retained independently
of the SDK handler's bounded drain, so an SDK cancellation cannot falsely release
an executing native worker. The adapter also waits for SDK service-instance
lifetimes to end. A trusted callback that blocks beyond cancellation may delay a
true cleanup receipt; it never becomes a false success.

`serve_http(application, listener, config, verifier, stop)` borrows a prebound
Tokio listener's application/runtime and owns accepted connections until joined
shutdown. Cancel `stop` and await the serving future. Application close also stops
the endpoint. Explicit adapter close leaves a borrowed application usable; the
CLI joins the endpoint, then application close and termination, checks that all
admitted work released, and drops its runtime outside async before exit. SIGINT
and Unix SIGTERM are graceful, including during blocked output. A request's
invalid input, credential, deadline or disconnect does not stop the listener.
Endpoint configuration, accept and owned-task failures remain errors. Dropping a
serving future is not a join receipt. `Report::delivered` means Hyper completed
writing to the socket; it does not assert that a remote application consumed the
bytes.

## Qualification

```sh
cargo +1.99.1 nextest run --locked -p rusty-haystack-mcp --features http
cargo +1.99.1 nextest run --locked -p rusty-haystack-mcp --features stdio,http
cargo +1.99.1 nextest run --locked -p rusty-haystack-cli --features mcp-http
cargo +1.99.1 nextest run --locked -p rusty-haystack-cli --features mcp,mcp-http
uv run --locked --project scripts/schema-client --python 3.12 python scripts/mcp/qualify_http.py
uv run --locked --project scripts/schema-client --python 3.12 python scripts/mcp/qualify_http.py --features mcp,mcp-http
```

The installed fixture creates its own temporary RSA issuer key, signs tokens with
OpenSSL, installs a fresh CLI, independently validates tool schemas with the
locked JSON Schema validator, and observes real HTTP sockets and processes. It
covers authentication, discovery, scoped reads, headers, input limits, signals
with blocked output, EOF, port reuse, configuration errors and cleanup. The
combined installation also runs the existing stdio fixture. `--binary PATH` is
an explicit fast mode and supplies no fresh-install evidence. Rust tests use the
official SDK client plus independent raw HTTP, real held native workers, slow
header/body peers, admission saturation, socket backpressure and owner/borrowed
close. These are local protocol/runtime observations, not production OAuth/TLS,
external host or model-call qualification.
