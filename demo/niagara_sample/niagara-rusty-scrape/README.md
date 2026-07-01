# Niagara nHaystack vs rusty-haystack scrape

Small Rust smoke test that compares how [rusty-haystack](https://github.com/jscott3201/rusty-haystack) talks to a Project Haystack server versus how Niagara nHaystack expects clients to authenticate.

Example lab target (see `env.example` for placeholders):

```
https://<jace-host>/haystack
user: <haystack-user>
auth: HTTP Basic (Niagara HTTPBasicScheme)
TLS: secure by default; pass --insecure-tls for self-signed lab certs only
```

Related tutorial (curl + reqwest baseline): [nhaystack-niagara-pi-tutorial](https://github.com/bbartling/py-bacnet-stacks-playground/tree/develop/vibe_code_apps_17/nhaystack-niagara-pi-tutorial)

## Setup

```bash
cd demo/niagara_sample/niagara-rusty-scrape
cp env.example .env
# edit .env — set JACE_HOST, HAYSTACK_USER, HAYSTACK_PASS
source .env
cargo run -- --insecure-tls --probe-scram
```

## What it checks

| Step | Path | Expected on Niagara |
|------|------|---------------------|
| 1 | GET `/about` no auth | `200` or `302` → `/login` (station dependent) |
| 1b | GET `/about` strict TLS | transport error on self-signed cert |
| 2 | GET `/about` HTTP Basic | `200` + Zinc grid |
| 3 | GET `/about` SCRAM HELLO | `401` HTML, **no** `WWW-Authenticate: SCRAM` |
| 4 | `HaystackClient::connect()` | fails — strict TLS + SCRAM-only auth |
| 5 | GET `/read?filter=point and cur` Basic + CSV | writes `nhaystack_points.csv` |

## TLS and auth flags

| Flag | Default | Meaning |
|------|---------|---------|
| `--tls-verify` | `true` | Verify server certificate and hostname |
| `--insecure-tls` | off | Lab only — disables cert **and** hostname checks |
| `--auth basic` | yes | Niagara nHaystack (HTTP Basic) |
| `--auth scram` | | SkySpark / rusty-haystack server |

When `--insecure-tls` is used with Basic auth, the tool prints an explicit MITM warning.

## Known gaps (addressed in this PR)

This demo exercises the new `ClientConfig` / `AuthMode::Basic` path in `haystack-client`:

- **Self-signed HTTPS** — use `--insecure-tls` (not the default)
- **HTTP Basic vs SCRAM** — use `--auth basic` for Niagara; `--probe-scram` shows why SCRAM fails
