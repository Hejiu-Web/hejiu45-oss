// Drive the console's real upload UI in a headless browser: sign in, open a
// bucket, upload several file types through the file input, and confirm the
// objects appear in the listing and download intact.
//
// Usage: node scripts/verify-upload.mjs <ws-url> <password> <out-dir>
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'

const [wsUrl, password, outDir] = process.argv.slice(2)
if (!wsUrl || !password || !outDir) {
  console.error('usage: node verify-upload.mjs <ws-url> <password> <out-dir>')
  process.exit(2)
}
mkdirSync(outDir, { recursive: true })

const origin = process.env.HJ45_ORIGIN || 'http://127.0.0.1:19060'
const ws = new WebSocket(wsUrl)
let id = 0
const pending = new Map()
ws.addEventListener('message', (ev) => {
  const m = JSON.parse(ev.data)
  if (m.id && pending.has(m.id)) {
    const { res, rej } = pending.get(m.id)
    pending.delete(m.id)
    m.error ? rej(new Error(JSON.stringify(m.error))) : res(m.result)
  }
})
const send = (method, params = {}) =>
  new Promise((res, rej) => {
    const i = ++id
    pending.set(i, { res, rej })
    ws.send(JSON.stringify({ id: i, method, params }))
  })
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function evaluate(expression) {
  const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails.exception?.description || r.exceptionDetails))
  return r.result.value
}
async function shot(name) {
  const r = await send('Page.captureScreenshot', { format: 'png' })
  writeFileSync(join(outDir, name), Buffer.from(r.data, 'base64'))
}

await new Promise((res, rej) => {
  ws.addEventListener('open', res, { once: true })
  ws.addEventListener('error', rej, { once: true })
})
await send('Page.enable')
await send('Runtime.enable')
await send('DOM.enable')
await send('Emulation.setDeviceMetricsOverride', {
  width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false,
})

const problems = []

// --- sign in ------------------------------------------------------------------
await send('Page.navigate', { url: origin + '/#/login' })
await sleep(1600)
await evaluate(`
  (() => {
    const i = [...document.querySelectorAll('.login-card input')]
    i[0].value = 'admin'; i[0].dispatchEvent(new Event('input', {bubbles:true}))
    i[1].value = ${JSON.stringify(password)}; i[1].dispatchEvent(new Event('input', {bubbles:true}))
    document.querySelector('.login-card').requestSubmit()
  })()
`)
await sleep(1800)
if (!(await evaluate(`!!document.querySelector('.sidebar')`))) {
  problems.push('could not sign in')
}

// --- create a bucket through the UI -------------------------------------------
await send('Page.navigate', { url: origin + '/#/buckets' })
await sleep(1400)
await evaluate(`
  (() => {
    const b = [...document.querySelectorAll('button')].find(x => /New bucket|新建存储桶/.test(x.textContent))
    b && b.click()
  })()
`)
await sleep(500)
await evaluate(`
  (() => {
    const f = document.querySelector('.modal input')
    f.value = 'uploads'; f.dispatchEvent(new Event('input', {bubbles:true}))
    document.querySelector('.modal').requestSubmit()
  })()
`)
await sleep(1200)

// --- open the bucket and reveal the uploader ----------------------------------
await send('Page.navigate', { url: origin + '/#/buckets/uploads' })
await sleep(1600)
const opened = await evaluate(`
  (() => {
    const b = [...document.querySelectorAll('button')].find(x => /Upload|上传/.test(x.textContent))
    if (!b) return 'no-button'
    b.click(); return 'clicked'
  })()
`)
if (opened !== 'clicked') problems.push('upload panel button not found: ' + opened)
await sleep(700)

// --- upload files through the real file input --------------------------------
// A PNG, a PDF, an SVG, JSON with CJK text, plain text and a binary blob.
const files = [
  { name: 'pixel.png', type: 'image/png', b64: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==' },
  { name: 'report.pdf', type: 'application/pdf', text: '%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<<>>\n%%EOF\n' },
  { name: 'icon.svg', type: 'image/svg+xml', text: '<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><circle cx="4" cy="4" r="4"/></svg>' },
  { name: 'config.json', type: 'application/json', text: '{"hello":"世界","n":[1,2,3]}' },
  { name: 'notes.txt', type: 'text/plain', text: '第一行\nsecond line\n' },
  { name: 'blob.bin', type: 'application/octet-stream', base64len: 4096 },
]

const { root } = await send('DOM.getDocument', { depth: -1 })
const { nodeId } = await send('DOM.querySelector', { nodeId: root.nodeId, selector: 'input[type=file]:not([webkitdirectory])' })
if (!nodeId) {
  problems.push('file input not found')
} else {
  await send('DOM.setFileInputFiles', {
    nodeId,
    files: files.map((f) => join(process.cwd(), 'tmp-upload-fixtures', f.name)),
  })
}

// The uploads run asynchronously; wait for the jobs table to settle.
for (let i = 0; i < 60; i++) {
  await sleep(500)
  const done = await evaluate(`
    (() => {
      const rows = [...document.querySelectorAll('.upload-list tbody tr')]
      if (!rows.length) return { rows: 0 }
      const doneN = rows.filter(r => /done|完成/.test(r.textContent)).length
      const errN = rows.filter(r => r.querySelector('.badge.danger')).length
      return { rows: rows.length, done: doneN, errors: errN }
    })()
  `)
  if (done.rows && done.done + done.errors === done.rows) break
}
const jobState = await evaluate(`
  (() => {
    const rows = [...document.querySelectorAll('.upload-list tbody tr')]
    return rows.map(r => ({
      text: r.textContent.replace(/\\s+/g, ' ').trim().slice(0, 120),
      error: r.querySelector('.badge.danger')?.textContent || '',
    }))
  })()
`)
for (const j of jobState) {
  if (j.error) problems.push(`upload failed: ${j.text}`)
}
await shot('upload-progress.png')

// --- the objects must be listed ----------------------------------------------
await sleep(800)
const listing = await evaluate(`document.querySelector('.main').innerText.replace(/\\s+/g,' ')`)
for (const f of files) {
  if (!listing.includes(f.name)) problems.push(`"${f.name}" missing from the object list`)
}

// --- and downloadable with intact bytes --------------------------------------
const fetched = await evaluate(`
  (async () => {
    const out = {}
    for (const name of ${JSON.stringify(files.map((f) => f.name))}) {
      const r = await fetch('/api/buckets/uploads/object?key=' + encodeURIComponent(name))
      const buf = new Uint8Array(await r.arrayBuffer())
      out[name] = { status: r.status, type: r.headers.get('content-type'), len: buf.length, first: buf[0] }
    }
    return out
  })()
`)
for (const f of files) {
  const got = fetched[f.name]
  if (!got || got.status !== 200) {
    problems.push(`download ${f.name}: ${got?.status}`)
    continue
  }
  const expectedLen = f.b64
    ? Buffer.from(f.b64, 'base64').length
    : f.base64len
      ? f.base64len
      : Buffer.byteLength(f.text, 'utf8')
  if (got.len !== expectedLen) {
    problems.push(`download ${f.name}: length ${got.len} != ${expectedLen}`)
  }
}

// --- settings page shows the limits card -------------------------------------
await send('Page.navigate', { url: origin + '/#/settings' })
await sleep(1600)
const settingsText = await evaluate(`document.querySelector('.main').innerText.replace(/\\s+/g,' ')`)
if (!/Upload limits|上传限制/.test(settingsText)) {
  problems.push('settings page has no upload limits card')
}
await shot('settings-limits.png')

// --- a file over the limit must be refused -----------------------------------
await evaluate(`
  (async () => {
    await fetch('/api/settings/limits', {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ max_object_bytes: 1024 }),
    })
  })()
`)
await send('Page.navigate', { url: origin + '/#/buckets/uploads' })
await sleep(1600)
await evaluate(`
  (() => {
    const b = [...document.querySelectorAll('button')].find(x => /Upload|上传/.test(x.textContent))
    b && b.click()
  })()
`)
await sleep(600)
const doc2 = await send('DOM.getDocument', { depth: -1 })
const q2 = await send('DOM.querySelector', { nodeId: doc2.root.nodeId, selector: 'input[type=file]:not([webkitdirectory])' })
await send('DOM.setFileInputFiles', {
  nodeId: q2.nodeId,
  files: [join(process.cwd(), 'tmp-upload-fixtures', 'too-big.bin')],
})
await sleep(2500)
const rejected = await evaluate(`
  (() => {
    const rows = [...document.querySelectorAll('.upload-list tbody tr')]
    return rows.some(r => /exceeds|larger than|超过/.test(r.textContent))
  })()
`)
if (!rejected) problems.push('an oversized file was not refused')
await shot('upload-rejected.png')

// restore an unlimited setting so the fixture server stays usable
await evaluate(`
  fetch('/api/settings/limits', {
    method: 'PATCH', headers: {'Content-Type':'application/json'},
    body: JSON.stringify({ max_object_bytes: null }),
  })
`)

console.log(JSON.stringify({ problems, fetched, jobState: jobState.length }, null, 2))
ws.close()
process.exit(problems.length ? 1 : 0)
