<script setup>
import { computed, onMounted, ref, watch } from 'vue'
import { api, toast } from '../store.js'
import { bytes, copy, dateTime } from '../format.js'
import { t } from '../i18n.js'
import Uploader from '../components/Uploader.vue'

const props = defineProps({ name: { type: String, required: true } })

const bucket = ref(null)
const objects = ref([])
const prefixes = ref([])
const keys = ref([])
const limits = ref({ max_object_bytes: null, part_bytes: 8 * 1024 * 1024 })
const loading = ref(true)
const error = ref('')
const prefix = ref('')
const searching = ref(false)
const showUploader = ref(false)

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
    if (!bucket.value) throw new Error(t('detail.bucketNotFound'))
    // Limits drive the uploader's part size and early size check.
    try {
      limits.value = await api.get('/api/settings/limits')
    } catch {
      /* non-fatal: fall back to the compiled-in defaults */
    }
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
  const nextVis = bucket.value.visibility === 'public-read' ? 'private' : 'public-read'
  try {
    await api.patch(`/api/buckets/${encodeURIComponent(props.name)}`, { visibility: nextVis })
    bucket.value.visibility = nextVis
    toast(nextVis === 'private' ? t('detail.nowPrivate') : t('detail.nowPublic'), 'ok')
  } catch (e) {
    toast(e.message, 'error')
  }
}

async function makePresigned(obj) {
  if (!presignFor.value) {
    toast(t('detail.pickKey'), 'error')
    return
  }
  try {
    const r = await api.post(
      `/api/buckets/${encodeURIComponent(props.name)}/objects/presign?key=${encodeURIComponent(obj.key)}`,
      { access_key_id: presignFor.value, ttl_seconds: Number(presignTtl.value) },
    )
    presigned.value = r.url
    toast(t('detail.urlReady'), 'ok')
  } catch (e) {
    toast(e.message, 'error')
  }
}

async function doCopy(text) {
  if (await copy(text)) toast(t('keys.copied'), 'ok')
  else toast(t('detail.copyFailed'), 'error')
}

/** Delete one object through the console API. */
async function removeObj(o) {
  if (!confirm(t('detail.confirmDelete', { key: o.key }))) return
  try {
    await api.del(
      `/api/buckets/${encodeURIComponent(props.name)}/objects?key=${encodeURIComponent(o.key)}`,
    )
    toast(t('detail.deleted', { key: o.key }), 'ok')
    await load()
  } catch (e) {
    toast(e.message, 'error', 6000)
  }
}

/** Download (or open) an object via the session-authenticated endpoint. */
function openObject(o, download) {
  const q = new URLSearchParams({ key: o.key })
  if (download) q.set('download', '1')
  window.open(`/api/buckets/${encodeURIComponent(props.name)}/object?${q}`, '_blank', 'noopener')
}

/** Whether the browser can render this type inline usefully. */
function canPreview(o) {
  const ct = (o.content_type || '').toLowerCase()
  return (
    ct.startsWith('image/') ||
    ct.startsWith('text/') ||
    ct === 'application/pdf' ||
    ct.startsWith('video/') ||
    ct.startsWith('audio/')
  )
}

async function onUploaded() {
  await load()
}

watch(() => props.name, load)
onMounted(load)
</script>

<template>
  <div class="page-head">
    <div>
      <div class="muted" style="font-size: 12px; margin-bottom: 4px">
        <router-link to="/buckets">{{ t('nav.buckets') }}</router-link> /
      </div>
      <h1 class="mono">{{ name }}</h1>
      <p v-if="bucket">
        {{ t('detail.created', { when: dateTime(bucket.created_at) }) }} ·
        {{ bucket.object_count }} {{ t('buckets.colObjects') }} ·
        {{ bytes(bucket.total_bytes) }}
      </p>
    </div>
    <div class="row" v-if="bucket">
      <span class="badge" :class="bucket.visibility === 'public-read' ? 'warn' : 'ok'">
        {{ t('buckets.visibility.' + bucket.visibility) }}
      </span>
      <button class="small" @click="toggleVisibility">
        {{ bucket.visibility === 'public-read' ? t('detail.makePrivate') : t('detail.makePublic') }}
      </button>
    </div>
  </div>

  <div v-if="error" class="alert error">{{ error }}</div>

  <div class="card" v-if="bucket">
    <div class="card-head">
      <h2>{{ t('upload.title') }}</h2>
      <button class="small" @click="showUploader = !showUploader">
        {{ showUploader ? t('detail.dismiss') : t('upload.dropTitle') }}
      </button>
    </div>
    <Uploader
      v-if="showUploader"
      :bucket="name"
      :prefix="prefix"
      :max-bytes="limits.max_object_bytes"
      :part-bytes="limits.part_bytes"
      :access-keys="keys"
      @uploaded="onUploaded"
    />
    <p v-else class="dim" style="margin: 0">
      {{ t('upload.howTo') }}
      <span v-if="limits.max_object_bytes">
        {{ t('upload.limitNote', { max: bytes(limits.max_object_bytes) }) }}
      </span>
    </p>
  </div>

  <div class="card" v-if="bucket">
    <div class="card-head">
      <h2>{{ t('detail.objects') }}</h2>
      <div class="row">
        <input
          v-model="prefix"
          :placeholder="t('detail.filterPlaceholder')"
          style="width: 220px"
          @keyup.enter="loadObjects"
        />
        <button class="small" @click="loadObjects" :disabled="searching">
          {{ searching ? '…' : t('detail.filter') }}
        </button>
      </div>
    </div>

    <div class="row" v-if="prefix" style="margin-bottom: 10px">
      <button class="small ghost" @click="up">↑ {{ t('detail.parent') }}</button>
      <span class="mono muted">{{ prefix }}</span>
    </div>

    <table v-if="prefixes.length || objects.length">
      <thead>
        <tr>
          <th>{{ t('detail.colKey') }}</th>
          <th>{{ t('detail.colType') }}</th>
          <th class="num">{{ t('detail.colSize') }}</th>
          <th>{{ t('detail.colModified') }}</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="p in prefixes" :key="'p-' + p">
          <td>
            <a href="#" class="mono" @click.prevent="enter(p)">📁 {{ p }}</a>
          </td>
          <td><span class="badge">{{ t('detail.prefix') }}</span></td>
          <td class="num muted">—</td>
          <td class="muted">—</td>
          <td></td>
        </tr>
        <tr v-for="o in objects" :key="o.key">
          <td class="mono truncate" :title="o.key">
            <a href="#" @click.prevent="openObject(o, false)">{{ o.key }}</a>
          </td>
          <td class="muted">{{ o.content_type }}</td>
          <td class="num">{{ bytes(o.size) }}</td>
          <td class="muted">{{ dateTime(o.last_modified) }}</td>
          <td class="row end">
            <button v-if="canPreview(o)" class="small ghost" @click="openObject(o, false)">
              {{ t('detail.preview') }}
            </button>
            <button class="small ghost" @click="openObject(o, true)">{{ t('detail.download') }}</button>
            <button class="small" @click="makePresigned(o)">{{ t('detail.share') }}</button>
            <button class="small danger" @click="removeObj(o)">{{ t('detail.delete') }}</button>
          </td>
        </tr>
      </tbody>
    </table>
    <div v-else class="empty">
      {{ t('detail.empty', { under: prefix ? t('detail.emptyUnder', { prefix }) : '' }) }}
    </div>
  </div>

  <div class="card" v-if="bucket">
    <div class="card-head"><h2>{{ t('detail.access') }}</h2></div>
    <p class="dim" style="margin-top: 0">
      {{ t('detail.accessLead') }}
    </p>
    <table v-if="grants.length">
      <thead>
        <tr>
          <th>{{ t('detail.colAccessKey') }}</th>
          <th>{{ t('detail.colLabel') }}</th>
          <th>{{ t('detail.colRead') }}</th>
          <th>{{ t('detail.colWrite') }}</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="g in grants" :key="g.access_key">
          <td class="mono">{{ g.access_key }}</td>
          <td>{{ g.label }}</td>
          <td><span class="badge" :class="g.read ? 'ok' : ''">{{ g.read ? t('detail.yes') : t('detail.no') }}</span></td>
          <td><span class="badge" :class="g.write ? 'ok' : ''">{{ g.write ? t('detail.yes') : t('detail.no') }}</span></td>
        </tr>
      </tbody>
    </table>
    <div v-else class="empty">
      {{ t('detail.noAccess') }} <router-link to="/keys">{{ t('detail.keysPage') }}</router-link> {{ t('detail.keysPageSuffix') }}
    </div>
  </div>

  <div class="card" v-if="bucket">
    <div class="card-head"><h2>{{ t('detail.shareTitle') }}</h2></div>
    <div class="grid cols-3">
      <label class="field">
        <span>{{ t('detail.signingKey') }}</span>
        <select v-model="presignFor">
          <option value="">{{ t('detail.choose') }}</option>
          <option v-for="g in readableKeys" :key="g.access_key" :value="g.access_key">
            {{ g.label }} ({{ g.access_key }})
          </option>
        </select>
      </label>
      <label class="field">
        <span>{{ t('detail.lifetime') }}</span>
        <select v-model="presignTtl">
          <option :value="300">{{ t('detail.min5') }}</option>
          <option :value="900">{{ t('detail.min15') }}</option>
          <option :value="3600">{{ t('detail.hour1') }}</option>
          <option :value="86400">{{ t('detail.day1') }}</option>
        </select>
      </label>
    </div>
    <p class="hint" style="margin-bottom: 12px">
      {{ t('detail.shareHint') }}
    </p>
    <div v-if="presigned">
      <pre class="code">{{ presigned }}</pre>
      <div class="row" style="margin-top: 10px">
        <button class="small" @click="doCopy(presigned)">{{ t('detail.copyUrl') }}</button>
        <button class="small ghost" @click="presigned = ''">{{ t('detail.dismiss') }}</button>
      </div>
    </div>
  </div>
</template>
