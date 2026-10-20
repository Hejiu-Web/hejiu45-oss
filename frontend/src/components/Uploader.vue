<script setup>
import { computed, onBeforeUnmount, reactive, ref } from 'vue'
import { api, toast } from '../store.js'
import { t } from '../i18n.js'
import {
  cancelUpload,
  createJob,
  formatBytes,
  formatEta,
  formatSpeed,
  uploadJob,
} from '../uploader.js'

const props = defineProps({
  bucket: { type: String, required: true },
  /** Current prefix, so dropped files land where the user is browsing. */
  prefix: { type: String, default: '' },
  /** Server-enforced limit; used to refuse obviously oversized files early. */
  maxBytes: { type: [Number, null], default: null },
  partBytes: { type: Number, default: 8 * 1024 * 1024 },
  accessKeys: { type: Array, default: () => [] },
})
const emit = defineEmits(['uploaded'])

const jobs = ref([])
const dragging = ref(false)
const busy = ref(false)
const mode = ref('console')
const accessKeyId = ref('')

const fileInput = ref(null)
const dirInput = ref(null)

const writableKeys = computed(() => props.accessKeys.filter((k) => (k.grants || []).some((g) => g.write)))

const active = computed(() => jobs.value.filter((j) => j.status === 'uploading' || j.status === 'pending'))
const finished = computed(() => jobs.value.filter((j) => j.status === 'done' || j.status === 'error' || j.status === 'cancelled'))
const overall = computed(() => {
  const list = active.value
  if (!list.length) return null
  const total = list.reduce((n, j) => n + j.size, 0)
  const loaded = list.reduce((n, j) => n + j.loaded, 0)
  return { total, loaded, pct: total ? Math.floor((loaded / total) * 100) : 0 }
})

// Track a rolling rate per job so the UI can show speed and ETA.
const ticker = setInterval(() => {
  const now = Date.now()
  for (const j of jobs.value) {
    if (j.status !== 'uploading') continue
    const elapsed = (now - j.startedAt) / 1000
    j.speed = elapsed > 0 ? j.loaded / elapsed : 0
  }
}, 500)
onBeforeUnmount(() => clearInterval(ticker))

function keyFor(file, path = '') {
  const rel = path ? `${path.replace(/^\/+/, '')}${file.name}` : file.name
  return `${props.prefix}${rel}`
}

function addFiles(fileList, path = '') {
  const picked = []
  for (const file of fileList) {
    // Ignore empty placeholders some browsers emit for directory entries.
    if (!file || (file.size === 0 && !file.type && !file.name)) continue
    picked.push(file)
  }
  if (!picked.length) return

  if (props.maxBytes != null) {
    const tooBig = picked.filter((f) => f.size > props.maxBytes)
    if (tooBig.length) {
      toast(
        t('upload.rejectedCount', { count: tooBig.length, max: formatBytes(props.maxBytes) }),
        'error',
        7000,
      )
    }
  }

  for (const file of picked) {
    jobs.value.push(createJob(file, keyFor(file, path), mode.value, accessKeyId.value, props.bucket))
  }
  void run()
}

/** Walk a dropped directory so folder uploads keep their structure. */
async function addEntry(entry, path = '') {
  if (entry.isFile) {
    const file = await new Promise((res, rej) => entry.file(res, rej))
    addFiles([file], path)
    return
  }
  if (!entry.isDirectory) return
  const reader = entry.createReader()
  const readBatch = () =>
    new Promise((res, rej) => reader.readEntries(res, rej))
  let batch = await readBatch()
  while (batch.length) {
    for (const child of batch) {
      await addEntry(child, `${path}${entry.name}/`)
    }
    batch = await readBatch()
  }
}

async function onDrop(e) {
  dragging.value = false
  const dt = e.dataTransfer
  if (!dt) return
  // Modern browsers expose webkitGetAsEntry, which is the only way to keep the
  // directory structure of a dropped folder.
  const entries = []
  if (dt.items && dt.items.length) {
    for (const item of dt.items) {
      const entry = item.webkitGetAsEntry?.()
      if (entry) entries.push(entry)
    }
  }
  if (entries.length) {
    for (const entry of entries) await addEntry(entry)
  } else if (dt.files?.length) {
    addFiles(dt.files)
  }
}

function pickFiles() {
  fileInput.value?.click()
}

function pickDir() {
  dirInput.value?.click()
}

function onInput(e) {
  addFiles(e.target.files)
  e.target.value = ''
}

function onDirInput(e) {
  const files = [...(e.target.files || [])]
  // webkitRelativePath looks like "folder/sub/file.txt"; strip the top folder
  // because the drop path already reflects where the user is browsing.
  const groups = new Map()
  for (const f of files) {
    const rel = f.webkitRelativePath || f.name
    const parts = rel.split('/')
    parts.pop()
    const dir = parts.length ? parts.join('/') + '/' : ''
    if (!groups.has(dir)) groups.set(dir, [])
    groups.get(dir).push(f)
  }
  for (const [dir, list] of groups) addFiles(list, dir)
  e.target.value = ''
}

const controllers = new Map()

async function run() {
  if (busy.value) return
  busy.value = true
  try {
    for (const job of jobs.value) {
      if (job.status !== 'pending') continue
      const controller = new AbortController()
      controllers.set(job.id, controller)
      try {
        await uploadJob(job, {
          partBytes: props.partBytes,
          maxBytes: props.maxBytes,
          signal: controller.signal,
          onProgress: () => {},
        })
      } catch {
        /* recorded on the job */
      } finally {
        controllers.delete(job.id)
      }
    }
  } finally {
    busy.value = false
    if (jobs.value.some((j) => j.status === 'done')) emit('uploaded')
  }
}

async function cancel(job) {
  controllers.get(job.id)?.abort()
  await cancelUpload(job)
}

function clearFinished() {
  jobs.value = jobs.value.filter((j) => j.status === 'uploading' || j.status === 'pending')
}

const anyPresignedKey = computed(() => writableKeys.value.length > 0)
</script>

<template>
  <div
    class="uploader"
    :class="{ dragging }"
    @dragenter.prevent="dragging = true"
    @dragover.prevent="dragging = true"
    @dragleave.prevent="dragging = false"
    @drop.prevent="onDrop"
  >
    <input ref="fileInput" type="file" multiple hidden @change="onInput" />
    <input ref="dirInput" type="file" webkitdirectory directory multiple hidden @change="onDirInput" />

    <div class="upload-drop">
      <div class="upload-icon" aria-hidden="true">⇪</div>
      <div>
        <strong>{{ t('upload.dropTitle') }}</strong>
        <div class="hint" style="margin-top: 4px">
          {{ t('upload.dropHint') }}
          <template v-if="maxBytes"> {{ t('upload.limitNote', { max: formatBytes(maxBytes) }) }}</template>
          <template v-else> {{ t('upload.noLimitNote') }}</template>
        </div>
      </div>
      <div class="row" style="gap: 8px">
        <button class="small primary" type="button" @click="pickFiles">{{ t('upload.chooseFiles') }}</button>
        <button class="small" type="button" @click="pickDir">{{ t('upload.chooseFolder') }}</button>
      </div>
    </div>

    <div class="upload-modes">
      <label class="row" style="gap: 6px">
        <input type="radio" value="console" v-model="mode" style="width: auto" />
        {{ t('upload.modeConsole') }}
      </label>
      <label class="row" style="gap: 6px" :title="anyPresignedKey ? '' : t('upload.needWriteKey')">
        <input
          type="radio"
          value="presigned"
          v-model="mode"
          style="width: auto"
          :disabled="!anyPresignedKey"
        />
        {{ t('upload.modePresigned') }}
      </label>
      <select
        v-if="mode === 'presigned'"
        v-model="accessKeyId"
        style="max-width: 260px"
      >
        <option value="">{{ t('detail.choose') }}</option>
        <option v-for="k in writableKeys" :key="k.access_key_id" :value="k.access_key_id">
          {{ k.label }} ({{ k.access_key_id }})
        </option>
      </select>
    </div>

    <div v-if="overall" class="upload-overall">
      <div class="upload-bar"><div class="upload-fill" :style="{ width: overall.pct + '%' }"></div></div>
      <div class="hint">
        {{ t('upload.overall', { done: formatBytes(overall.loaded), total: formatBytes(overall.total), pct: overall.pct }) }}
      </div>
    </div>

    <table v-if="jobs.length" class="upload-list">
      <thead>
        <tr>
          <th>{{ t('upload.colFile') }}</th>
          <th class="num">{{ t('upload.colSize') }}</th>
          <th>{{ t('upload.colProgress') }}</th>
          <th class="num">{{ t('upload.colSpeed') }}</th>
          <th class="num">{{ t('upload.colEta') }}</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="j in jobs" :key="j.id">
          <td class="mono truncate" :title="j.key">{{ j.key }}</td>
          <td class="num muted">{{ formatBytes(j.size) }}</td>
          <td>
            <div class="upload-bar small">
              <div
                class="upload-fill"
                :class="{ ok: j.status === 'done', bad: j.status === 'error' }"
                :style="{ width: (j.size ? Math.floor((j.loaded / j.size) * 100) : 0) + '%' }"
              ></div>
            </div>
            <span v-if="j.status === 'error'" class="badge danger">{{ j.error }}</span>
            <span v-else-if="j.status === 'cancelled'" class="badge">{{ t('upload.cancelled') }}</span>
            <span v-else-if="j.status === 'done'" class="badge ok">{{ t('upload.done') }}</span>
            <span v-else class="hint">{{ Math.floor((j.size ? j.loaded / j.size : 0) * 100) }}%</span>
          </td>
          <td class="num muted">{{ formatSpeed(j.speed) }}</td>
          <td class="num muted">
            {{ j.status === 'uploading' && j.speed ? formatEta((j.size - j.loaded) / j.speed) : '—' }}
          </td>
          <td class="row end">
            <button
              v-if="j.status === 'uploading' || j.status === 'pending'"
              class="small danger"
              type="button"
              @click="cancel(j)"
            >
              {{ t('upload.cancel') }}
            </button>
          </td>
        </tr>
      </tbody>
    </table>

    <div v-if="finished.length && !active.length" class="row end" style="margin-top: 10px">
      <button class="small ghost" type="button" @click="clearFinished">{{ t('upload.clear') }}</button>
    </div>

    <div v-if="dragging" class="upload-overlay">{{ t('upload.dropOverlay') }}</div>
  </div>
</template>
