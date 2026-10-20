// Screenshot the console behind the session cookie via the Chrome DevTools
// Protocol. Used for visual verification during development; not part of the
// shipped product.
//
// Usage: node scripts/screenshot-console.mjs <ws-url> <session> <out-dir>
//   ws-url   page target webSocketDebuggerUrl from http://127.0.0.1:9222/json
//   session  hj45_session cookie value
//   out-dir  directory for the PNGs
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'

const [wsUrl, session, outDir] = process.argv.slice(2)
if (!wsUrl || !session || !outDir) {
  console.error('usage: node screenshot-console.mjs <ws-url> <session> <out-dir>')
  process.exit(2)
}
mkdirSync(outDir, { recursive: true })

const origin = 'http://127.0.0.1:19005'
const ws = new WebSocket(wsUrl)
let id = 0
const pending = new Map()

function send(method, params = {}) {
  return new Promise((resolve, reject) => {
    const msgId = ++id
    pending.set(msgId, { resolve, reject })
    ws.send(JSON.stringify({ id: msgId, method, params }))
  })
}

ws.addEventListener('message', (ev) => {
  const msg = JSON.parse(ev.data)
  if (msg.id && pending.has(msg.id)) {
    const { resolve, reject } = pending.get(msg.id)
    pending.delete(msg.id)
    if (msg.error) reject(new Error(JSON.stringify(msg.error)))
    else resolve(msg.result)
  }
})

const wait = (ms) => new Promise((r) => setTimeout(r, ms))

ws.addEventListener('open', async () => {
  try {
    await send('Page.enable')
    await send('Network.enable')
    // HttpOnly cookies can only be set through CDP, not document.cookie.
    await send('Network.setCookie', {
      name: 'hj45_session',
      value: session,
      domain: '127.0.0.1',
      path: '/',
      httpOnly: true,
      secure: false,
    })
    await send('Emulation.setDeviceMetricsOverride', {
      width: 1360,
      height: 900,
      deviceScaleFactor: 1,
      mobile: false,
    })

    const pages = [
      ['overview', '/#/'],
      ['buckets', '/#/buckets'],
      ['keys', '/#/keys'],
      ['audit', '/#/audit'],
      ['settings', '/#/settings'],
    ]
    for (const [name, path] of pages) {
      await send('Page.navigate', { url: origin + path })
      await wait(1500)
      const shot = await send('Page.captureScreenshot', { format: 'png' })
      const file = join(outDir, `${name}.png`)
      writeFileSync(file, Buffer.from(shot.data, 'base64'))
      console.log(`wrote ${file}`)
    }
  } catch (e) {
    console.error('failed:', e.message)
    process.exitCode = 1
  } finally {
    ws.close()
  }
})
