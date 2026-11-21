# rusty-haystack-mcp

Optional read-only MCP adapter over `haystack-app`. There are no default
features. Select `stdio` to expose `serve`, `StdioConfig`, `Limits`, `McpError` and `Report`.
The adapter borrows the managed application/runtime and offers
`haystack_query` and `haystack_read` using the shared policy, cursors and budgets.
An explicitly selected history service adds the optional
[`haystack_history` tool](../docs/mcp-history.md), with finite per-call limits,
retained-record completeness and joined provider cleanup.
Explicit [H4 semantic resources](../docs/mcp-semantic-resources.md) expose bounded
definition/library discovery and exact reads through the same application policy,
with content fixed to H4 independently of the entity-tool output profile.

See [the MCP pipe profile](../docs/mcp-stdio.md) for launch configuration, schemas,
lifecycle guarantees, protocol versions and qualification commands. Standalone
consumers install `rusty-haystack-cli` with its `mcp` feature and launch
`haystack mcp stdio --config FILE` with Unix pipe stdin/stdout.

Select `http` for the authenticated local Streamable HTTP profile and
`serve_http`, `HttpConfig`, `JwtVerifier`, `JwtConfig` and `VerificationKey`.
The CLI selects this with `mcp-http` and runs `haystack mcp http --config FILE`.
See [the HTTP profile](../docs/mcp-http.md) for explicit loopback authority,
offline issuer/key/scope configuration, protocol bounds and joined cleanup.
Both features may be selected together; neither is enabled by default.
