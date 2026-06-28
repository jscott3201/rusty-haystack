# Niagara nHaystack vs rusty-haystack scrape

Small Rust smoke test that compares how [rusty-haystack](https://github.com/jscott3201/rusty-haystack) talks to a Project Haystack server versus how Niagara nHaystack expects clients to authenticate.

Target station (lab):

```
https://192.168.204.11/haystack
user: open_fdd
auth: HTTP Basic (Niagara HTTPBasicScheme)
```

Related tutorial (curl + reqwest baseline): [nhaystack-niagara-pi-tutorial](https://github.com/bbartling/py-bacnet-stacks-playground/tree/develop/vibe_code_apps_17/nhaystack-niagara-pi-tutorial)

## Setup

```bash
cd /home/ben/haystack_samples/niagara-rusty-scrape
cp env.example .env
# edit .env — set HAYSTACK_PASS
source .env
cargo run
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

## Known gaps

Two blockers for using `rusty-haystack-client` against this Niagara lab as-is:

1. **Self-signed HTTPS** — `haystack-client` builds a default `reqwest` client with strict certificate verification. Niagara’s lab cert fails unless you use mTLS config with a custom CA or add an insecure/dev TLS option (what `curl -k` does in the [pi tutorial](https://github.com/bbartling/py-bacnet-stacks-playground/tree/develop/vibe_code_apps_17/nhaystack-niagara-pi-tutorial)).

2. **HTTP Basic vs SCRAM** — Niagara nHaystack authenticates with HTTP Basic on every request. `rusty-haystack-client` only implements Project Haystack SCRAM SHA-256 (`HELLO` → `SCRAM` → `BEARER`). Niagara returns HTML `401` with no `WWW-Authenticate: SCRAM` challenge.

Possible follow-ups:

- Add HTTP Basic transport mode to `haystack-client`
- Or put a SCRAM-capable Haystack proxy in front of Niagara
- Or use Open-FDD's existing Haystack driver (basic auth) until rusty-haystack grows Niagara support
