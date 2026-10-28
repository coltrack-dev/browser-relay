<script setup lang="ts">
import { onMounted, ref } from 'vue'

type SessionState = 'stopped' | 'starting' | 'running' | 'stopping' | 'failed'
type Status = { state: SessionState; url: string | null }

const url = ref('http://fixture/index.html')
const status = ref<Status>({ state: 'stopped', url: null })
const error = ref('')
const busy = ref(false)
const video = ref<HTMLVideoElement | null>(null)
const mediaState = ref('idle')
let mediaSocket: WebSocket | null = null
let peer: RTCPeerConnection | null = null
let reconnectTimer: ReturnType<typeof setTimeout> | null = null
let intentionalMediaClose = false

async function request(path: string, init?: RequestInit) {
  error.value = ''
  const response = await fetch(path, init)
  const body = await response.json() as Status
  if (!response.ok) throw new Error(`HTTP ${response.status}: ${body.state}`)
  return body
}

async function refresh() { status.value = await request('/api/session') }
async function action(operation: () => Promise<Status>) {
  busy.value = true
  try { status.value = await operation() } catch (cause) { error.value = cause instanceof Error ? cause.message : 'Неизвестная ошибка' } finally { busy.value = false }
}
async function connectVideo() {
  if (mediaSocket || status.value.state !== 'running') return
  intentionalMediaClose = false
  const protocol = location.protocol === 'https:' ? 'wss' : 'ws'
  mediaSocket = new WebSocket(`${protocol}://${location.host}/api/media/ws`)
  peer = new RTCPeerConnection({ iceServers: [] })
  peer.addTransceiver('video', { direction: 'recvonly' })
  peer.ontrack = event => { if (video.value) video.value.srcObject = event.streams[0] }
  peer.oniceconnectionstatechange = () => { mediaState.value = peer?.iceConnectionState ?? 'closed' }
  peer.onconnectionstatechange = () => { if (peer?.connectionState === 'failed') error.value = 'WebRTC connection failed' }
  peer.onicecandidate = event => { if (event.candidate && mediaSocket?.readyState === WebSocket.OPEN) mediaSocket.send(JSON.stringify({ type: 'ice_candidate', candidate: event.candidate.candidate, sdp_m_line_index: event.candidate.sdpMLineIndex ?? 0 })) }
  mediaSocket.onopen = async () => { const offer = await peer?.createOffer(); if (offer && peer && mediaSocket) { await peer.setLocalDescription(offer); mediaSocket.send(JSON.stringify({ type: 'offer', sdp: offer.sdp })) } }
  mediaSocket.onmessage = async event => { const message = JSON.parse(event.data) as { type: string; sdp?: string; state?: string; message?: string }; if (message.type === 'answer' && message.sdp && peer) await peer.setRemoteDescription({ type: 'answer', sdp: message.sdp }); if (message.type === 'state' && message.state) mediaState.value = message.state; if (message.type === 'error') error.value = message.message ?? 'Media signaling error' }
  mediaSocket.onclose = () => { mediaSocket = null; peer?.close(); peer = null; if (!intentionalMediaClose && status.value.state === 'running') { mediaState.value = 'reconnecting'; reconnectTimer = setTimeout(() => { reconnectTimer = null; void connectVideo() }, 1000) } else mediaState.value = 'idle' }
}
function disconnectVideo() { intentionalMediaClose = true; if (reconnectTimer) { clearTimeout(reconnectTimer); reconnectTimer = null }; mediaSocket?.close(); mediaSocket = null; peer?.close(); peer = null; mediaState.value = 'idle'; if (video.value) video.value.srcObject = null }
async function start() { await action(() => request('/api/session', { method: 'POST' })); await connectVideo() }
async function stop() { disconnectVideo(); await action(() => request('/api/session/stop', { method: 'POST' })) }
function navigate() { return action(() => request('/api/session/navigate', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ url: url.value }) })) }
function reload() { return action(() => request('/api/session/reload', { method: 'POST' })) }
onMounted(refresh)
</script>

<template>
  <main class="shell">
    <section class="card">
      <p class="eyebrow">PHASE 1 · LOCAL BROWSER RUNTIME</p>
      <h1>BrowserRelay</h1>
      <p class="muted">Управление одной локальной Chromium-сессией через CDP.</p>
      <label for="url">URL</label>
      <div class="url-row"><input id="url" v-model="url" type="url" :disabled="busy" @keyup.enter="navigate"><button :disabled="busy || status.state !== 'running'" @click="navigate">Перейти</button></div>
      <div class="actions"><button :disabled="busy || status.state === 'running'" @click="start">Запустить</button><button :disabled="busy || status.state !== 'running'" @click="reload">Reload</button><button class="danger" :disabled="busy || status.state !== 'running'" @click="stop">Остановить</button></div>
      <button class="video-toggle" :disabled="status.state !== 'running' || !!mediaSocket" @click="connectVideo">Подключить видео</button>
      <video ref="video" class="remote-video" autoplay playsinline muted></video>
      <p class="media-state">Видео: {{ mediaState }}</p>
      <p class="state" :class="status.state"><span></span>{{ status.state }}</p>
      <p v-if="status.url" class="current-url">{{ status.url }}</p>
      <p v-if="error" class="error" role="alert">{{ error }}</p>
    </section>
  </main>
</template>
