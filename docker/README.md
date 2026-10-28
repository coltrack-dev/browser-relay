# Docker и Chromium smoke test

Конфигурация использует официальные `rust`, `debian` и `nginx` images. Chromium, Xvfb и GStreamer (`ximagesrc`, H.264 `x264enc`, RTP payloader, `webrtcbin` и обязательный ICE plugin `gstreamer1.0-nice` с `nicesrc`/`nicesink`) устанавливаются из Debian packages. `webrtcbin` требует libnice для ICE negotiation. Backend и Chromium работают от непривилегированного пользователя `browserrelay`; `--no-sandbox`, `privileged`, дополнительные capabilities и host namespace настройки не используются.

Запуск:

```sh
docker compose up -d --build
./docker/smoke-test.sh
```

Chromium получает SOCKS5 из `BROWSER_RELAY_PROXY` (default `socks5://192.168.13.128:4712`). Локальные `localhost`, loopback и hostname `fixture` обходят proxy; внешние адреса не получают автоматический DIRECT fallback. Скопируйте `.env.example` в `.env` для настройки compose.

Smoke test использует `fixture` внутри compose-сети: внешний сайт не нужен. Он проверяет start, CDP navigation, reload, наличие Chromium процесса и cleanup после stop.

Проверка sandbox:

```sh
docker compose exec relay sh -c 'id && chromium --version'
docker compose exec relay sh -c 'chromium --headless=new --dump-dom http://fixture/index.html'
docker compose exec relay gst-inspect-1.0 webrtcbin ximagesrc x264enc rtph264pay nicesrc nicesink

# Полный Phase 2A smoke test (Docker + Playwright)
./docker/phase2a-smoke-test.sh
```

Если Chromium сообщает об ограничениях sandbox/user namespaces, конфигурация должна быть исправлена на уровне совместимого образа или runtime-хоста. Нельзя добавлять `--no-sandbox`, `privileged` или менять host security settings.

Smoke test проверяет Xvfb, наличие GStreamer plugins, запуск frontend, SDP/ICE signaling, получение video track с ненулевым размером и остановку сессии. SDP и ICE содержимое в обычные логи не выводится.
