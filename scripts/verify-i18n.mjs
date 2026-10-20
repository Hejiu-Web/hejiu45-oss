// Verify the console renders in both languages, by driving it through the
// Chrome DevTools Protocol: set the session cookie, load each page, read the
// visible text, and save PNGs for visual review.
//
// Usage: node scripts/verify-i18n.mjs <ws-url> <session> <out-dir>
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'

const [wsUrl, session, outDir] = process.argv.slice(2)
if (!wsUrl || !session || !outDir) {
  console.error('usage: node verify-i18n.mjs <ws-url> <session> <out-dir>')
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
    msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result)
  }
})

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function evaluate(expression) {
  const r = await send('Runtime.evaluate', {
    expression,
    awaitPromise: true,
    returnByValue: true,
  })
  if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails))
  return r.result.value
}

async function goto(path) {
  await send('Page.navigate', { url: origin + path })
  await sleep(1200)
}

/** Switch language by clicking the visible control (no storage poking). */
async function switchTo(code) {
  // The control is labelled differently by position: the sidebar uses the short
  // form ("EN" / "中"), the login page the full name ("English" / "中文").
  const labels = code === 'zh' ? ['中文', '中'] : ['English', 'EN']
  const ok = await evaluate(`
    (() => {
      const wanted = ${JSON.stringify(labels)}
      const btn = [...document.querySelectorAll('.lang-btn')]
        .find(b => wanted.includes(b.textContent.trim()))
      if (!btn) return false
      btn.click()
      return true
    })()
  `)
  if (!ok) throw new Error(`language button for "${code}" not found on this page`)
  await sleep(450)
}

async function shot(name) {
  const r = await send('Page.captureScreenshot', { format: 'png' })
  const file = join(outDir, name)
  writeFileSync(file, Buffer.from(r.data, 'base64'))
  return file
}

async function bodyText() {
  return evaluate(`document.body.innerText.replace(/\\s+/g, ' ').slice(0, 600)`)
}

await new Promise((res, rej) => {
  ws.addEventListener('open', res, { once: true })
  ws.addEventListener('error', rej, { once: true })
})

await send('Page.enable')
await send('Runtime.enable')
await send('Network.enable')

// Authenticate by planting the session cookie for the origin.
await send('Network.setCookie', {
  name: 'hj45_session',
  value: session,
  domain: '127.0.0.1',
  path: '/',
})
await send('Emulation.setDeviceMetricsOverride', {
  width: 1440,
  height: 900,
  deviceScaleFactor: 1,
  mobile: false,
})

const problems = []
const results = {}

// Sign in first via the real form; the login page itself is checked at the end
// by signing out, which guarantees the SPA is actually on that route.
//
// Navigate to the bare origin: a hard load of `#/login` while a session cookie
// is present redirects to the overview, so the form would not be on screen.
await goto('/')
await sleep(600)
// Assigning `.value` then dispatching `input` is what Vue's v-model listens
// for; the prototype setter cannot be invoked directly across CDP realms.
await evaluate(`
  (async () => {
    const inputs = [...document.querySelectorAll('.login-card input')]
    if (inputs.length < 2) return 'no-form'
    inputs[0].value = 'admin'
    inputs[0].dispatchEvent(new Event('input', { bubbles: true }))
    inputs[1].value = ${JSON.stringify(process.env.HJ45_PASSWORD || '')}
    inputs[1].dispatchEvent(new Event('input', { bubbles: true }))
    document.querySelector('.login-card').requestSubmit()
    return 'submitted'
  })()
`)
await sleep(1500)
const afterLogin = await bodyText()
if (!afterLogin.includes('概览') && !afterLogin.includes('Overview')) {
  problems.push(`could not sign in via the form -> ${afterLogin.slice(0, 160)}`)
}

// --- authenticated pages --------------------------------------------------------
const pages = [
  ['overview', '/#/', ['Overview', '概览'], ['Buckets', '存储桶']],
  ['buckets', '/#/buckets', ['Buckets', '存储桶'], ['Visibility', '可见性']],
  ['keys', '/#/keys', ['Access keys', '访问密钥'], ['Issue a key', '签发新密钥']],
  ['audit', '/#/audit', ['Audit log', '审计日志'], ['Actor', '操作者']],
  ['settings', '/#/settings', ['Settings', '设置'], ['Change password', '修改密码']],
  ['detail', '/#/buckets/photos', ['Objects', '对象'], ['Access', '访问权限']],
]

for (const [name, path, enNeedle, zhNeedle] of pages) {
  await goto(path)

  // Ensure the page is in English by clicking the control if needed.
  let text = await bodyText()
  if (text.includes('概览') || text.includes('访问密钥')) {
    await switchTo('en')
    text = await bodyText()
  }
  const en = text
  if (!en.includes(enNeedle[0])) {
    problems.push(`${name} (en): missing "${enNeedle[0]}" -> ${en.slice(0, 140)}`)
  }
  await shot(`${name}-en.png`)

  // Switch to Chinese through the UI.
  await switchTo('zh')
  const zh = await bodyText()
  if (!zh.includes(zhNeedle[1])) {
    problems.push(`${name} (zh): missing "${zhNeedle[1]}" -> ${zh.slice(0, 140)}`)
  }
  if (zh.includes(zhNeedle[0]) && zhNeedle[0] !== zhNeedle[1]) {
    problems.push(`${name} (zh): still shows English "${zhNeedle[0]}"`)
  }
  await shot(`${name}-zh.png`)
  results[name] = { en: en.slice(0, 60), zh: zh.slice(0, 60) }
}

// The choice must survive a reload.
await goto('/#/buckets')
await sleep(700)
const persisted = await bodyText()
if (!persisted.includes('存储桶')) {
  problems.push('language choice did not persist across a reload')
}
// --- login page, both languages ------------------------------------------------
// Reach it by clicking the real sign-out control, so the SPA navigates there
// with its own state machine rather than being forced by a URL.
await evaluate(`
  (() => {
    const btn = [...document.querySelectorAll('button')]
      .find(b => /Sign out|退出登录/.test(b.textContent))
    if (!btn) return false
    btn.click()
    return true
  })()
`)
await sleep(1200)
const onLogin = await evaluate(`!!document.querySelector('.login-card')`)
if (!onLogin) problems.push('sign-out did not reach the login page')

// English
if ((await bodyText()).includes('用户名')) await switchTo('en')
const loginEn = await bodyText()
if (!loginEn.includes('Sign in') || !loginEn.includes('Username')) {
  problems.push(`login (en): unexpected text -> ${loginEn.slice(0, 160)}`)
}
await shot('login-en.png')

// Chinese
await switchTo('zh')
const loginZh = await bodyText()
if (!loginZh.includes('登录') || !loginZh.includes('用户名')) {
  problems.push(`login (zh): unexpected text -> ${loginZh.slice(0, 160)}`)
}
if (loginZh.includes('Sign in')) problems.push('login (zh) still shows English "Sign in"')
await shot('login-zh.png')
results.login = { en: loginEn.slice(0, 80), zh: loginZh.slice(0, 80) }

const htmlLang = await evaluate(`document.documentElement.lang`)
if (!htmlLang.startsWith('zh')) problems.push(`<html lang> is "${htmlLang}" after switching to zh`)

console.log(JSON.stringify({ problems, htmlLang, results }, null, 2))
ws.close()
process.exit(problems.length ? 1 : 0)
