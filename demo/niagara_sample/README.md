# Niagara nHaystack demo (rusty-haystack)

Example lab target: `https://<jace-host>/haystack` (Niagara 4.15 + nHaystack 3.3).

Bench notes below document one Open-FDD field deployment (2026-06); substitute your JACE host and credentials via `env.example`.

## Observed Niagara station (example bench — N4.15 lab)

| Field | Value |
|-------|--------|
| **Platform** | **Niagara 4 (N4)** — Tridium |
| **Niagara build** | **4.15.3.28** (`productVersion` from `/about`) |
| **Station name** | `v4Fifteen` (`serverName`) |
| **nHaystack module** | **3.3.0.0** (`moduleVersion`) |
| **Haystack protocol** | **2.0** (`haystackVersion` in about grid) |
| **Product** | Niagara 4 (`productName`) |
| **Time zone** | `America/Chicago` (`tz`) |
| **HTTPS endpoint** | `https://192.168.204.11/haystack` |
| **Servlet name** | `haystack` (`NHaystackService` in Workbench) |
| **Auth (API user)** | HTTP Basic — `HTTPBasicScheme` (not DigestScheme, not Haystack SCRAM) |
| **TLS** | Self-signed station cert — use `--insecure-tls` on the demo (secure default is strict verification) |
| **BACnet driver** | `BacnetNetwork` → device **`BENS-BENCHTEST-BOX`** |
| **Example slot path** | `@C.Drivers.BacnetNetwork.BENS-BENCHTEST-BOX.points.OA~2dT` |

Example **`/about`** row (Zinc):

```
ver:"3.0"
productName: Niagara 4
productVersion: 4.15.3.28
moduleName: nhaystack
moduleVersion: 3.3.0.0
serverName: v4Fifteen
haystackVersion: 2.0
```

Example **current-value points** (same bench as BACnet device 5007 on MSTP):

| dis | curVal (approx) | unit |
|-----|-----------------|------|
| OA-H | ~51 %RH | %RH |
| OA-T | ~72 °F | °F |
| DUCT-T | ~65–69 °F | °F |
| DUCT-P | ~-0.14 | in/wc |
| STAT ZN-T | ~73 °F | °F |
| ACTUATOR-POS | ~0.58 | % |
| ACTUATOR-0 | 0 | % |

Ethernet adapter on the Windows station host: **192.168.204.11** (same subnet as bench `enp3s0` @ 192.168.204.55/24).

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
3. **Strict TLS** — Niagara lab cert requires `--insecure-tls` (same as `curl -k`; not the default).

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
