import { chromium } from 'playwright'

const browser = await chromium.launch({ headless: true })
const page = await browser.newPage({ viewport: { width: 1280, height: 720 } })
await page.goto('http://web/', { waitUntil: 'networkidle' })
await page.getByRole('button', { name: 'Запустить' }).click()
await page.getByText('running', { exact: true }).waitFor()
await page.locator('video').evaluate(video => new Promise((resolve, reject) => {
  const deadline = Date.now() + 15000
  let frames = 0
  const check = () => {
    const tracks = video.srcObject instanceof MediaStream ? video.srcObject.getVideoTracks() : []
    const quality = video.getVideoPlaybackQuality?.()
    const decodedFrames = quality?.totalVideoFrames ?? 0
    if (video.videoWidth > 0 && video.videoHeight > 0 && tracks.some(track => track.readyState === 'live') && (decodedFrames > 0 || frames > 0)) {
      resolve()
      return
    }
    if (Date.now() >= deadline) {
      reject(new Error(`video did not deliver decoded frames (width=${video.videoWidth}, height=${video.videoHeight}, decoded=${decodedFrames}, callbacks=${frames})`))
      return
    }
    if ('requestVideoFrameCallback' in video) {
      video.requestVideoFrameCallback(() => { frames += 1; check() })
    } else {
      setTimeout(check, 100)
    }
  }
  check()
}))
await page.getByRole('button', { name: 'Остановить' }).click()
await page.getByText('stopped', { exact: true }).waitFor()
await browser.close()
console.log('BrowserRelay Phase 2A WebRTC smoke test passed')
