#!/usr/bin/env node
// Verify that every language has the same set of message keys, and that every
// key referenced from a template or script actually exists.
//
// Run with `npm run check:i18n` (also part of `npm run build`), so a missing
// translation fails the build instead of showing a raw key like "keys.title"
// to a user.

import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join, dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const srcDir = join(root, 'src')

// Import the message tables by evaluating the module's export. The file uses
// ESM and no Vue-specific syntax for `messages`, so a dynamic import works.
const mod = await import(new URL('../src/i18n.js', import.meta.url).href)
const messages = mod.messages
const locales = Object.keys(messages)

let problems = 0
const fail = (msg) => {
  console.error(`  ✗ ${msg}`)
  problems++
}

// 1. Same key set in every locale.
const [base, ...others] = locales
const baseKeys = new Set(Object.keys(messages[base]))
for (const loc of others) {
  const keys = new Set(Object.keys(messages[loc]))
  for (const k of baseKeys) if (!keys.has(k)) fail(`${loc}: missing key "${k}"`)
  for (const k of keys) if (!baseKeys.has(k)) fail(`${loc}: extra key "${k}"`)
}

// 2. No empty values.
for (const loc of locales) {
  for (const [k, v] of Object.entries(messages[loc])) {
    if (typeof v !== 'string' || v.trim() === '') fail(`${loc}: "${k}" is empty`)
  }
}

// 3. Every literal key used in the source exists. Keys built by concatenation
//    (e.g. 'buckets.visibility.' + x) are listed in `dynamicKeys`.
const dynamicKeys = new Set(['buckets.visibility.private', 'buckets.visibility.public-read'])

function walk(dir) {
  const out = []
  for (const name of readdirSync(dir)) {
    const p = join(dir, name)
    if (statSync(p).isDirectory()) out.push(...walk(p))
    else if (/\.(vue|js)$/.test(name) && name !== 'i18n.js') out.push(p)
  }
  return out
}

const keyRe = /\bt2?\(\s*'([a-zA-Z0-9_.]+)'/g
const used = new Set()
for (const file of walk(srcDir)) {
  const text = readFileSync(file, 'utf8')
  for (const m of text.matchAll(keyRe)) used.add(m[1])
}

// A key that ends in "." is a concatenation prefix (`'buckets.visibility.' +
// value`), not a key itself; validate the prefixes that are actually used.
const prefixes = [...used].filter((k) => k.endsWith('.'))
for (const p of prefixes) {
  used.delete(p)
  const variants = [...baseKeys].filter((k) => k.startsWith(p))
  if (variants.length === 0) fail(`concatenated prefix "${p}" matches no keys`)
}

for (const k of used) {
  if (!baseKeys.has(k)) fail(`unknown key used in source: "${k}"`)
}
// Keys that exist but are never referenced are usually leftovers; report them
// as a warning rather than failing, since some are reached via concatenation.
const unused = [...baseKeys].filter(
  (k) => !used.has(k) && !dynamicKeys.has(k) && !prefixes.some((p) => k.startsWith(p)),
)

if (problems > 0) {
  console.error(`\ni18n check failed: ${problems} problem(s)`)
  process.exit(1)
}
console.log(
  `i18n ok: ${baseKeys.size} keys × ${locales.length} languages, ${used.size} referenced` +
    (unused.length ? `, ${unused.length} unreferenced (${unused.join(', ')})` : ''),
)
