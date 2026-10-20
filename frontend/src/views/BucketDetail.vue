<script setup>
import { computed, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { api, toast } from '../store.js'
import { bytes, copy, dateTime } from '../format.js'

const props = defineProps({ name: { type: String, required: true } })
const router = useRouter()

const bucket = ref(null)
const objects = ref([])
const prefixes = ref([])
const keys = ref([])
const loading = ref(true)
const error = ref('')
const prefix = ref('')
const searching = ref(false)

const presignFor = ref('')
const presignTtl = ref(900)
const presigned = ref('')

const grants = computed(() => bucket.value?.grants || [])
const readableKeys = computed(() => grants.value.filter((g) => g.read))

async function load() {
  loading.value = true
  error.value = ''
  try {
    const [b, k] = await Promise.all([api.get('/api/buckets'), api.get('/api/keys')])
    bucket.value = (b.buckets || []).find((x) => x.name === props.name) || null
    keys.value = k.keys || []
    if (!bucket.value) throw new Error('Bucket not found')
    await loadObjects()
  } catch (e) {
    error.value = e.message
  } finally {
    loading.value = false
  }
}

async function loadObjects() {
  searching.value = true
  try {
    const q = new URLSearchParams({ prefix: prefix.value, delimiter: '/' })
    const r = await api.get(`/api/buckets/${encodeURIComponent(props.name)}/objects?${q}`)
    objects.value = r.objects || []
    prefixes.value = r.common_prefixes || []
  } catch (e) {
    error.value = e.message
  } finally {
    searching.value = false
  }
}

function enter(p) {
  prefix.value = p
  loadObjects()
}

function up() {
  const parts = prefix.value.split('/').filter(Boolean)
  parts.pop()
  prefix.value = parts.length ? parts.join('/') + '/' : ''
  loadObjects()
}

async function toggleVisibility() {
  const next = bucket.value.visibility === 'public-read' ? 'private' : 'public-read'
  try {
    await api.patch(`/api/buckets/${encodeURIComponent(props.name)}`, { visibility: next })
    bucket.value.visibility = next
    toast(`Bucket is now ${next}.`, 'ok')
  } catch (e) {
    toast(e.message, 'error')
  }
}

async function makePresigned(obj) {
  if (!presignFor.value) {
    toast('Pick an access key first (it needs read permission on this bucket).', 'error')
    return
  }
  try {
    const r = await api.post(
      `/api/buckets/${encodeURIComponent(props.name)}/objects/presign?key=${encodeURIComponent(obj.key)}`,
      { access_key_id: presignFor.value, ttl_seconds: Number(presignTtl.value) },
    )
    presigned.value = r.url
    toast('Signed URL ready — copy it now.', 'ok')
  } catch (e) {
    toast(e.message, 'error')
  }
}

async function doCopy(text) {
  if (await copy(text)) toast('Copied.', 'ok')
  else toast('Copy failed — select the text manually.', 'error')
}

async function removeObj(o) {
  if (!confirm(`Delete "${o.key}"?`)) return
  toast('Use the S3 API or an access key to delete objects (console deletes are disabled).', 'info', 6000)
}

watch(() => props.name, load)
onMounted(load)
</script>

<template>
  <div class="page-head">
    <div>
      <div class="muted" style="font-size: 12px; margin-bottom: 4px">
        <router-link to="/buckets">Buckets</router-link> /
      </div>
      <h1 class="mono">{{ name }}</h1>
      <p v-if="bucket">
        created {{ dateTime(bucket.created_at) }} · {{ bucket.object_count }} objects ·
        {{ bytes(bucket.total_bytes) }}
      </p>
    </div>
    <div class="row" v-if="bucket">
      <span class="badge" :class="bucket.visibility === 'public-read' ? 'warn' : 'ok'">
        {{ bucket.visibility }}
      </span>
      <button class="small" @click="toggleVisibility">
        Make {{ bucket.visibility === 'public-read' ? 'private' : 'public-read' }}
      </button>
    </div>
  </div>

  <div v-if="error" class="alert error">{{ error }}</div>

  <div class="card" v-if="bucket">
    <div class="card-head">
      <h2>Objects</h2>
      <div class="row">
        <input
          v-model="prefix"
          placeholder="prefix/filter"
          style="width: 220px"
          @keyup.enter="loadObjects"
        />
        <button class="small" @click="loadObjects" :disabled="searching">
          {{ searching ? '…' : 'Filter' }}
        </button>
      </div>
    </div>

    <div class="row" v-if="prefix" style="margin-bottom: 10px">
      <button class="small ghost" @click="up">↑ Parent</button>
      <span class="mono muted">{{ prefix }}</span>
    </div>

    <table v-if="prefixes.length || objects.length">
      <thead>
        <tr>
          <th>Key</th>
          <th>Type</th>
          <th class="num">Size</th>
          <th>Modified</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="p in prefixes" :key="'p-' + p">
          <td>
            <a href="#" class="mono" @click.prevent="enter(p)">📁 {{ p }}</a>
          </td>
          <td><span class="badge">prefix</span></td>
          <td class="num muted">—</td>
          <td class="muted">—</td>
          <td></td>
        </tr>
        <tr v-for="o in objects" :key="o.key">
          <td class="mono truncate" :title="o.key">{{ o.key }}</td>
          <td class="muted">{{ o.content_type }}</td>
          <td class="num">{{ bytes(o.size) }}</td>
          <td class="muted">{{ dateTime(o.last_modified) }}</td>
          <td class="row end">
            <button class="small" @click="makePresigned(o)">Share</button>
          </td>
        </tr>
      </tbody>
    </table>
    <div v-else class="empty">
      No objects{{ prefix ? ` under "${prefix}"` : '' }}. Upload with the S3 API
      (<code>aws s3 cp</code>) or drag files — uploads run over SigV4, never the session cookie.
    </div>
  </div>

  <div class="card" v-if="bucket">
    <div class="card-head"><h2>Access</h2></div>
    <p class="dim" style="margin-top: 0">
      Read and write are separate. A key with no grant cannot even list this bucket.
    </p>
    <table v-if="grants.length">
      <thead>
        <tr>
          <th>Access key</th>
          <th>Label</th>
          <th>Read</th>
          <th>Write</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="g in grants" :key="g.access_key">
          <td class="mono">{{ g.access_key }}</td>
          <td>{{ g.label }}</td>
          <td><span class="badge" :class="g.read ? 'ok' : ''">{{ g.read ? 'yes' : 'no' }}</span></td>
          <td><span class="badge" :class="g.write ? 'ok' : ''">{{ g.write ? 'yes' : 'no' }}</span></td>
        </tr>
      </tbody>
    </table>
    <div v-else class="empty">
      No key has access yet. Grant one on the <router-link to="/keys">Access keys</router-link> page.
    </div>
  </div>

  <div class="card" v-if="bucket">
    <div class="card-head"><h2>Share an object</h2></div>
    <div class="grid cols-3">
      <label class="field">
        <span>Signing key (needs read)</span>
        <select v-model="presignFor">
          <option value="">— choose —</option>
          <option v-for="g in readableKeys" :key="g.access_key" :value="g.access_key">
            {{ g.label }} ({{ g.access_key }})
          </option>
        </select>
      </label>
      <label class="field">
        <span>Lifetime</span>
        <select v-model="presignTtl">
          <option :value="300">5 minutes</option>
          <option :value="900">15 minutes</option>
          <option :value="3600">1 hour</option>
          <option :value="86400">1 day</option>
        </select>
      </label>
    </div>
    <p class="hint" style="margin-bottom: 12px">
      Click <strong>Share</strong> on a row above to mint a short-lived presigned URL. No
      permanent public links are ever created.
    </p>
    <div v-if="presigned">
      <pre class="code">{{ presigned }}</pre>
      <div class="row" style="margin-top: 10px">
        <button class="small" @click="doCopy(presigned)">Copy URL</button>
        <button class="small ghost" @click="presigned = ''">Dismiss</button>
      </div>
    </div>
  </div>
</template>
