#!/bin/sh
set -eu

base_url="${BROWSER_RELAY_SMOKE_URL:-http://127.0.0.1:${BROWSER_RELAY_PORT:-8099}}"
curl --fail --silent "$base_url/health" | grep -qx ok
curl --fail --silent -X POST "$base_url/api/session" | grep -q '"running"'
curl --fail --silent -X POST "$base_url/api/session/navigate" \
  -H 'content-type: application/json' \
  --data '{"url":"http://fixture/index.html"}' | grep -q 'fixture/index.html'
curl --fail --silent -X POST "$base_url/api/session/reload" | grep -q '"running"'
docker compose exec -T relay sh -c 'pgrep -x chromium >/dev/null'
curl --fail --silent -X POST "$base_url/api/session/stop" | grep -q '"stopped"'
if docker compose exec -T relay sh -c 'pgrep -x chromium >/dev/null'; then
  echo 'Chromium is still running after stop' >&2
  exit 1
fi
echo 'BrowserRelay Chromium smoke test passed'
