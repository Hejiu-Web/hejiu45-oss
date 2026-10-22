// Drive the new public-access features in a headless browser: switch a bucket
// to public-read, read the permanent prefix, bind a custom domain, and check the
// access guide renders code for every SDK.
//
// Usage: node scripts/verify-public.mjs <ws-url> <password> <out-dir>
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'

const [wsUrl, password, outDir] = process.argv.slice(2)
if (!wsUrl || !password || !outDir) {
  console.error('usage: node verify-public.mjs <ws-url> <password> <out-dir>')
  process.exit(2)
}
mkdirSync(outDir, { recursive: true })

const origin = process.env.HJ45_ORIGIN || 'http://127.0.0.1:19090'
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
  if (r.exceptionDetails) {
    throw new Error(r.exceptionDetails.exception?.description || JSON.stringify(r.exceptionDetails))
  }
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
await send('Emulation.setDeviceMetricsOverride', {
  width: 1440, height: 1100, deviceScaleFactor: 1, mobile: false,
})

const problems = []

// --- sign in ------------------------------------------------------------------
await send('Page.navigate', { url: origin + '/#/login' })
await sleep(1600)
// A previous run may have left a session cookie, in which case the console is
// already signed in and there is no login form to fill.
const hasForm = await evaluate(`!!document.querySelector('.login-card input')`)
if (hasForm) {
  await evaluate(`
    (() => {
      const i = [...document.querySelectorAll('.login-card input')]
      i[0].value = 'admin'; i[0].dispatchEvent(new Event('input', {bubbles:true}))
      i[1].value = ${JSON.stringify(password)}; i[1].dispatchEvent(new Event('input', {bubbles:true}))
      document.querySelector('.login-card').requestSubmit()
    })()
  `)
  await sleep(1800)
}
if (!(await evaluate(`!!document.querySelector('.sidebar')`))) problems.push('could not sign in')

// --- seed a bucket with one object via the console API --------------------------
const seeded = await evaluate(`
  (async () => {
    const post = (u, b) => fetch(u, {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(b||{})})
    await post('/api/buckets', { name: 'pub', visibility: 'public-read' })
    const png = Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='), c => c.charCodeAt(0))
    const r = await fetch('/api/buckets/pub/objects?key=hello.png', {
      method: 'PUT', headers: {'Content-Type':'image/png'}, body: png,
    })
    return { upload: r.status }
  })()
`)
if (seeded.upload !== 200) problems.push('seed upload failed: ' + seeded.upload)

// --- bucket page: public access card ------------------------------------------
await send('Page.navigate', { url: origin + '/#/buckets/pub' })
await sleep(1700)
const pubText = await evaluate(`document.querySelector('.main').innerText.replace(/\\s+/g,' ')`)
if (!/公开访问|Public access/.test(pubText)) problems.push('no public access card: ' + pubText.slice(0, 200))
// The prefix is `{base}/{bucket}/` before a domain is bound and
// `{scheme}://{domain}/` after, so accept either shape.
if (!/pub\/your-file|pub\/hello\.png|cdn\.example\.com\//.test(pubText)) {
  problems.push('no public URL prefix shown')
}
// The permanent link must carry no signature.
if (/X-Amz-Signature/.test(pubText)) problems.push('public card shows a signed URL')
await shot('public-bucket.png')

// Copying the prefix must yield a signature-free URL.
const copied = await evaluate(`
  (() => {
    const code = [...document.querySelectorAll('.public-prefix code')][0]
    return code ? code.textContent.trim() : ''
  })()
`)
if (!copied) problems.push('public prefix code element missing')
if (/X-Amz/.test(copied)) problems.push('public prefix contains a signature: ' + copied)

// --- the permanent link must actually serve the object -----------------------
const served = await evaluate(`
  (async () => {
    const r = await fetch('/pub/hello.png')
    const buf = new Uint8Array(await r.arrayBuffer())
    return { status: r.status, type: r.headers.get('content-type'), len: buf.length }
  })()
`)
if (served.status !== 200) problems.push(`short link returned ${served.status}`)
if (!served.type?.startsWith('image/png')) problems.push('short link content-type: ' + served.type)

// --- bind a custom domain -----------------------------------------------------
await evaluate(`
  (() => {
    const input = document.querySelector('.domain-bind input')
    input.value = 'cdn.example.com'
    input.dispatchEvent(new Event('input', {bubbles:true}))
  })()
`)
await sleep(200)
await evaluate(`
  (() => {
    const btn = [...document.querySelectorAll('.domain-bind button')]
      .find(b => /保存域名|Save domain/.test(b.textContent))
    btn && btn.click()
  })()
`)
await sleep(1200)
const afterBind = await evaluate(`document.querySelector('.main').innerText.replace(/\\s+/g,' ')`)
if (!/cdn\.example\.com/.test(afterBind)) problems.push('custom domain did not appear after save')
if (!/CNAME/.test(afterBind)) problems.push('no CNAME instruction shown')
await shot('public-domain.png')

// --- access guide: every SDK tab renders code ---------------------------------
const rawTabs = await evaluate(`
  JSON.stringify([...document.querySelectorAll('.main button')]
    .map(b => b.textContent.trim())
    .filter(x => /aws-cli|boto3|JavaScript|Go/.test(x)))
`)
const guideTabs = typeof rawTabs === 'string' ? JSON.parse(rawTabs) : rawTabs || []
for (const label of ['aws-cli', 'Python (boto3)', 'JavaScript', 'Go']) {
  if (!guideTabs.includes(label)) problems.push('missing guide tab: ' + label)
}
for (const label of ['Python (boto3)', 'JavaScript', 'Go', 'aws-cli']) {
  await evaluate(`
    (() => {
      const b = [...document.querySelectorAll('.main button')]
        .find(x => x.textContent.trim() === ${JSON.stringify(label)})
      b && b.click()
    })()
  `)
  await sleep(300)
  const code = await evaluate(`document.querySelector('.main pre.code')?.textContent || ''`)
  if (!code.includes('pub')) problems.push(`${label} snippet missing the bucket name`)
  if (!/endpoint|Endpoint/.test(code) && label !== 'Go') problems.push(`${label} snippet missing the endpoint`)
}
await shot('access-guide.png')

// --- private bucket shows the signed-URL path instead -------------------------
await evaluate(`
  (async () => {
    await fetch('/api/buckets', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify({name:'priv'})})
  })()
`)
await sleep(400)
await send('Page.navigate', { url: origin + '/#/buckets/priv' })
await sleep(1700)
const privText = await evaluate(`document.querySelector('.main').innerText.replace(/\\s+/g,' ')`)
if (!/分享对象|Share an object/.test(privText)) problems.push('private bucket has no share card')
if (/公开访问|Public access/.test(privText)) problems.push('private bucket wrongly shows the public card')
await shot('private-bucket.png')

console.log(JSON.stringify({ problems, served, copied: copied.slice(0, 80), guideTabs }, null, 2))
ws.close()
process.exit(problems.length ? 1 : 0)
