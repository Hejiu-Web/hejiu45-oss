// Formatting helpers shared by the views.

import { t as t2 } from './i18n.js'

export function bytes(n) {
  if (n === 0 || n == null) return '0 B'
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB']
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)))
  const v = n / 1024 ** i
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`
}

export function ago(iso) {
  if (!iso) return '—'
  const t = Date.parse(iso)
  if (Number.isNaN(t)) return iso
  const s = Math.max(0, (Date.now() - t) / 1000)
  if (s < 60) return t2('time.justNow')
  if (s < 3600) return t2('time.minutesAgo', { n: Math.floor(s / 60) })
  if (s < 86400) return t2('time.hoursAgo', { n: Math.floor(s / 3600) })
  if (s < 86400 * 30) return t2('time.daysAgo', { n: Math.floor(s / 86400) })
  return new Date(t).toISOString().slice(0, 10)
}

export function dateTime(iso) {
  if (!iso) return '—'
  const t = Date.parse(iso)
  if (Number.isNaN(t)) return iso
  return new Date(t).toISOString().replace('T', ' ').replace(/\.\d+Z$/, 'Z')
}

export function duration(seconds) {
  const s = Math.floor(seconds || 0)
  const d = Math.floor(s / 86400)
  const h = Math.floor((s % 86400) / 3600)
  const m = Math.floor((s % 3600) / 60)
  if (d) return `${d}d ${h}h`
  if (h) return `${h}h ${m}m`
  if (m) return `${m}m ${s % 60}s`
  return `${s}s`
}

export async function copy(text) {
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    // Fallback for non-secure contexts (plain HTTP on a LAN address).
    const ta = document.createElement('textarea')
    ta.value = text
    ta.style.position = 'fixed'
    ta.style.opacity = '0'
    document.body.appendChild(ta)
    ta.select()
    const ok = document.execCommand('copy')
    document.body.removeChild(ta)
    return ok
  }
}
