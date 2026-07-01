# Bench validation notes (2026-07-01)

Live validation on Open-FDD field bench after Windows firewall fix.

## Success criteria met

```bash
./scripts/check-niagara.sh
# TCP 192.168.204.11:443: OPEN
# HTTPS /about: OK — Niagara 4.15.3.28, nHaystack 3.3.0.0, serverName v4Fifteen

source .env && cargo run -- --insecure-tls --probe-scram
# preflight: TCP endpoint reachable
# SCRAM probe: 401, no WWW-Authenticate: SCRAM  (expected on nHaystack)
# read filter="point and cur" → 9 rows (BACnet + SomeRandomPoint)
```

## Windows firewall (required once per bench PC)

On Niagara host (`192.168.204.11`), **Admin PowerShell**:

```powershell
New-NetFirewallRule -DisplayName "Open-FDD bench Haystack 443" -Direction Inbound -Protocol TCP -LocalPort 443 -RemoteAddress 192.168.204.55 -Action Allow
```

ICMP ping may still fail; **TCP 443** is the gate that matters.

## Two different “Haystack” things in Workbench

| Component | Path | Role | This PR uses it? |
|-----------|------|------|------------------|
| **NHaystackService** servlet | Config → Services → WebService → `haystack` | **Inbound** REST API at `https://host/haystack` | **Yes** — `niagara-read` talks here |
| **RustyHaystack** | Config → Drivers → NHaystackNetwork → RustyHaystack | **Outbound** N Haystack **Server** driver (client to remote Haystack) | **No** — separate object |

**RustyHaystack `{down}`** with health `user cannot be empty string` means the **N Haystack Server** driver has **blank Username/Password** (see Property Sheet). That is unrelated to the working `/haystack` servlet API.

This PR did **not** create RustyHaystack — it was added manually in Workbench under `NHaystackNetwork`. To fix it (only if you need outbound Haystack client):

1. Open RustyHaystack Property Sheet
2. Set **Username** / **Password** (or disable the driver if unused)
3. Set **Internet Address** / **Uri Path** to the remote Haystack server you want to poll

For Open-FDD / `niagara-read`, you only need **NHaystackService** + **HTTPBasicScheme** on `open_fdd`.

## Points on this bench

| Point | Source | Haystack read | Write |
|-------|--------|---------------|-------|
| OA-T, OA-H, DUCT-T, … | BACnet `BENS-BENCHTEST-BOX` (device 5007) | ✓ via `read?filter=point and cur` | ACTUATOR-0 writable via BACnet/Haystack actions |
| **SomeRandomPoint** | `Config/haystack_tests` Numeric Writable | ✓ appears in read grid (`writable`) | Use Haystack `pointWrite` / Workbench Override (format TBD in demo) |

Slot path: `@C.haystack_tests.SomeRandomPoint`

## Open-FDD rigorous testing parallels

| Lesson | rusty-haystack demo | Open-FDD bench |
|--------|---------------------|----------------|
| TCP 443 blocked | `check-niagara.sh` / preflight fail | REV_325 Haystack driver `enabled=false` when station unreachable |
| Self-signed TLS | `--insecure-tls` | `tls_verify=false` in `local.nhaystack.toml` |
| HTTP Basic not SCRAM | `--auth basic` | `auth_mode=basic`, `OPENFDD_HAYSTACK_USER/PASS` |
| Windows firewall | Allow 443 from `192.168.204.55` | Same rule before `openfdd_rev325_rigorous_report.sh` Haystack phases |
| Placeholder `.env` | `<jace-host>` fails fast | WSL agents must set `data.env.local` before bootstrap |

Before overnight Open-FDD rigorous run:

```bash
cd ~/open-fdd
./scripts/openfdd_bench_pull_latest.sh   # when new containers land
cd ~/rusty-haystack/demo/niagara_sample/niagara-rusty-scrape && ./scripts/check-niagara.sh
OPENFDD_REV325_POLL_CYCLES=5 ./scripts/openfdd_rev325_rigorous_report.sh
```
