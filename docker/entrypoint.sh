#!/bin/sh
set -eu

Xvfb "${DISPLAY}" -screen 0 1280x720x24 -nolisten tcp >/tmp/browserrelay-xvfb.log 2>&1 &
xvfb_pid=$!
printf '%s\n' "$xvfb_pid" >/tmp/browserrelay-xvfb.pid
cleanup() {
  rm -f /tmp/browserrelay-xvfb.pid
  kill "$xvfb_pid" 2>/dev/null || true
}
trap cleanup EXIT INT TERM
kill -0 "$xvfb_pid"

exec /usr/local/bin/relay-server
