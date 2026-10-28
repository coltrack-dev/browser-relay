#!/bin/sh
set -eu

docker compose up -d --build relay fixture web
docker compose exec -T relay sh -c 'pgrep -x Xvfb >/dev/null && test "$DISPLAY" = :99 && gst-inspect-1.0 webrtcbin ximagesrc x264enc rtph264pay nicesrc nicesink >/dev/null'
smoke_image="${BROWSER_RELAY_PHASE2A_IMAGE:-browser-relay-phase2a-playwright:1.51.1}"
docker build \
  --tag "$smoke_image" \
  --file docker/playwright-smoke/Dockerfile \
  docker/playwright-smoke
docker run --rm --user pwuser --cap-drop ALL --security-opt no-new-privileges \
  --network browser-relay_default \
  "$smoke_image" \
  -e "import('playwright').then(() => console.log('Playwright import passed'))"
docker run --rm --user pwuser --cap-drop ALL --security-opt no-new-privileges \
  --network browser-relay_default \
  "$smoke_image" \
  /opt/browserrelay-phase2a/phase2a-browser-smoke.mjs
