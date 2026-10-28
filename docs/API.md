# HTTP API (Phase 1)

По умолчанию BrowserRelay доступен на `127.0.0.1:8099` и проксирует во внутренний порт контейнера `8080`. Внешний порт можно изменить через `BROWSER_RELAY_PORT`; bind остаётся ограничен `127.0.0.1`.

Исходящий трафик Chromium можно направить через SOCKS5 переменной `BROWSER_RELAY_PROXY`, например `socks5://192.168.13.128:4712`. Поддерживается только явный формат `socks5://host:port`; при ошибочном значении запуск Chromium завершается ошибкой и не переходит на `DIRECT`. Loopback, `localhost` и Docker fixture `fixture` обходят proxy. CDP `remote-debugging-pipe`, frontend, signaling и WebRTC ICE через этот proxy не направляются.

- `GET /health` — проверка HTTP-процесса.
- `GET /api/session` — состояние сессии.
- `POST /api/session` — запуск Chromium через `--remote-debugging-pipe`.
- `POST /api/session/stop` — CDP `Browser.close` и fallback cleanup процесса.
- `POST /api/session/navigate` с JSON `{ "url": "https://example.org" }` — CDP `Page.navigate`.
- `POST /api/session/reload` — CDP `Page.reload`.
- `GET /api/session/video-diagnostics` — read-only CDP `Runtime.evaluate` диагностика
  HTML5 video в уже запущенной Chromium-сессии. Выполняются три замера с интервалом
  2 секунды; ответ содержит `readyState`, `networkState`, `paused`, `currentTime`,
  `duration`, размеры, buffered ranges и decoded/dropped frame counters. Поле `success`
  становится `true` только при загрузке данных, активном воспроизведении, росте
  `currentTime` и росте `totalVideoFrames`; если playback quality API недоступен,
  используется `requestVideoFrameCallback`. При ошибке CDP ответ также содержит
  состояние Chromium: PID, exit code/signal, uptime, DISPLAY, состояние Xvfb и
  bounded stderr tail. Cookies, заголовки, токены и полный URL не возвращаются.
- `GET /api/media/ws` — WebSocket signaling для одного video viewer. Клиент отправляет `offer` и `ice_candidate`, backend возвращает `answer`, `state` и ошибки.

Поддерживаются только `http` и `https`. Video MVP использует Xvfb capture → H.264 → GStreamer `webrtcbin`; аудио и input control пока отсутствуют. Параметры видео: `BROWSER_RELAY_VIDEO_WIDTH`, `BROWSER_RELAY_VIDEO_HEIGHT`, `BROWSER_RELAY_VIDEO_FPS`, `BROWSER_RELAY_VIDEO_BITRATE_KBPS`.
