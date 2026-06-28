# Niagara nHaystack demo (rusty-haystack)

Live lab target: `https://192.168.204.11/haystack` (Niagara 4.15 + nHaystack 3.3).

## Quick start

```bash
cd demo/niagara_sample/niagara-rusty-scrape
cp env.example .env
# edit HAYSTACK_PASS
source .env
cargo run -- --probe-scram
```

## Auth modes

| Server | Auth | rusty-haystack flag |
|--------|------|---------------------|
| **Niagara nHaystack** | HTTP Basic (`HTTPBasicScheme` on service user) | `--auth basic` (default) |
| **SkySpark** | Haystack SCRAM SHA-256 | `--auth scram` |
| **rusty-haystack server** | Haystack SCRAM SHA-256 | `--auth scram` |

Niagara Workbench setup for API user:

```
Config → Services → AuthenticationService → add HTTPBasicScheme
Config → Services → UserService → open_fdd → Authentication Scheme Name: HTTPBasicScheme
NHaystackService → Servlet enabled, name: haystack
```

## What we proved on the bench

1. **`--auth basic`** — `HaystackClient::connect_with_config` + `ClientConfig::niagara_lab()` reads live BACnet points (OA-T, DUCT-T, …).
2. **`--auth scram`** / **`--probe-scram`** — Niagara returns `401` HTML with **no** `WWW-Authenticate: SCRAM`. nHaystack does not speak Project Haystack SCRAM today.
3. **Strict TLS** — Niagara lab cert requires `tls_verify: false` (same as `curl -k`).

## Library changes in this fork

`haystack-client` now exposes:

- `ClientConfig { tls_verify, auth_mode, … }`
- `AuthMode::Basic` — HTTP Basic on every request (Niagara)
- `AuthMode::Scram` — existing HELLO/SCRAM/BEARER (SkySpark, rusty-haystack server)
- `HaystackClient::connect_with_config()`

## Question for upstream (jscott3201)

> Discord suggested SCRAM works for Niagara and SkySpark out of the box.

Our live probe against **Niagara 4.15 / nHaystack 3.3** shows:

- HTTP Basic + insecure TLS → **works**
- SCRAM HELLO on `/haystack/about` → **401**, no `WWW-Authenticate: SCRAM`

Is there a different Niagara URL, nHaystack version, or Workbench auth scheme that enables Haystack SCRAM? Or does “Niagara” in that context mean a **Haystack proxy/gateway** in front of nHaystack rather than nHaystack itself?

Until clarified, use **`AuthMode::Basic`** for Niagara and **`AuthMode::Scram`** for SkySpark / rusty-haystack server.

## Test SCRAM locally

```bash
# terminal 1 — use a free port if 8080 is taken (e.g. Open-FDD bridge)
cargo run -p rusty-haystack-cli -- serve --demo --port 18080

# terminal 2
cargo run -p niagara-read -- \
  --url http://127.0.0.1:18080/api \
  --user admin --password admin \
  --auth scram --tls-verify \
  --filter site
```

(Use credentials from `demo/users.toml`.)
