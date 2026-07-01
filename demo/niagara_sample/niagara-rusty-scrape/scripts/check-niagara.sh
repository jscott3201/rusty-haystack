#!/usr/bin/env bash
# Quick Niagara nHaystack reachability check from the Open-FDD bench (192.168.204.55).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
[[ -f "$ROOT/.env" ]] && source "$ROOT/.env"

HOST="${JACE_HOST:-192.168.204.11}"
USER="${HAYSTACK_USER:-open_fdd}"
PASS="${HAYSTACK_PASS:-}"
BASE="${HAYSTACK_BASE:-https://${HOST}/haystack}"

echo "=== Niagara reachability from $(hostname -I | awk '{print $1}') ==="
echo "target: $BASE"
echo

if ping -c 1 -W 2 "$HOST" >/dev/null 2>&1; then
  echo "ping $HOST: OK"
else
  echo "ping $HOST: FAIL (ICMP often blocked on Windows — not fatal)"
fi

if timeout 3 bash -c "echo >/dev/tcp/${HOST}/443" 2>/dev/null; then
  echo "TCP ${HOST}:443: OPEN"
else
  echo "TCP ${HOST}:443: BLOCKED or station down"
  echo
  echo "Fix on Windows Niagara PC ($HOST):"
  echo "  - Services: nHaystack / station running"
  echo "  - Windows Firewall: inbound TCP 443 from 192.168.204.55"
  exit 1
fi

if [[ -z "$PASS" ]]; then
  echo "HAYSTACK_PASS not set — skip curl (set in .env)"
  exit 0
fi

if curl -kfsS -m 10 -u "${USER}:${PASS}" "${BASE%/}/about" | head -3; then
  echo
  echo "HTTPS /about: OK"
else
  echo "HTTPS /about: FAIL (auth or servlet path)"
  exit 1
fi
