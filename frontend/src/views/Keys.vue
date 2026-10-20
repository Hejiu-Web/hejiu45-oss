<script setup>
import { computed, onMounted, reactive, ref } from 'vue'
import { api, toast } from '../store.js'
import { ago, copy } from '../format.js'
import { t } from '../i18n.js'

const keys = ref([])
const buckets = ref([])
const loading = ref(true)
const error = ref('')

const creating = ref(false)
const newKey = ref({ label: '' })
const issued = ref(null)

const editing = ref(null)
const draftGrants = reactive({})

async function load() {
  loading.value = true
  try {
    const [k, b] = await Promise.all([api.get('/api/keys'), api.get('/api/buckets')])
    keys.value = k.keys || []
    buckets.value = b.buckets || []
  } catch (e) {
    error.value = e.message
  } finally {
    loading.value = false
  }
}

async function create() {
  creating.value = true
  error.value = ''
  try {
    issued.value = await api.post('/api/keys', { label: newKey.value.label.trim() })
    newKey.value.label = ''
    newKey.value = { label: '' }
    await load()
  } catch (e) {
    error.value = e.message
  } finally {
    creating.value = false
  }
}

async function rotate(k) {
  if (!confirm(t('keys.confirmRotate', { label: k.label }))) return
  try {
    const r = await api.post(`/api/keys/${encodeURIComponent(k.access_key_id)}/rotate`)
    issued.value = r
    toast(t('keys.rotated'), 'ok')
    await load()
  } catch (e) {
    toast(e.message, 'error')
  }
}

async function toggle(k) {
  try {
    await api.patch(`/api/keys/${encodeURIComponent(k.access_key_id)}`, { enabled: !k.enabled })
    await load()
  } catch (e) {
    toast(e.message, 'error')
  }
}

async function remove(k) {
  if (!confirm(t('keys.confirmDelete', { label: k.label }))) return
  try {
    await api.del(`/api/keys/${encodeURIComponent(k.access_key_id)}`)
    await load()
  } catch (e) {
    toast(e.message, 'error')
  }
}

function startEdit(k) {
  editing.value = k.access_key_id
  for (const name of Object.keys(draftGrants)) delete draftGrants[name]
  for (const b of buckets.value) {
    const g = (k.grants || []).find((x) => x.bucket === b.name)
    draftGrants[b.name] = { read: !!g?.read, write: !!g?.write }
  }
}

async function saveGrants(k) {
  const grants = Object.entries(draftGrants)
    .filter(([, v]) => v.read || v.write)
    .map(([bucket, v]) => ({ bucket, read: v.read, write: v.write }))
  try {
    await api.put(`/api/keys/${encodeURIComponent(k.access_key_id)}/grants`, { grants })
    toast(t('keys.grantsSaved'), 'ok')
    editing.value = null
    await load()
  } catch (e) {
    toast(e.message, 'error')
  }
}

async function doCopy(text) {
  if (await copy(text)) toast(t('keys.copied'), 'ok')
  else toast(t('keys.copyFailed'), 'error')
}

const issuedTitle = computed(() =>
  issued.value?.secret_access_key ? t('keys.storeSecret') : t('keys.actionComplete'),
)

onMounted(load)
</script>

<template>
  <div class="page-head">
    <div>
      <h1>{{ t('keys.title') }}</h1>
      <p>{{ t('keys.subtitle') }}</p>
    </div>
  </div>

  <div v-if="error" class="alert error">{{ error }}</div>

  <div class="card">
    <div class="card-head"><h2>{{ t('keys.issue') }}</h2></div>
    <form class="row" @submit.prevent="create">
      <input v-model="newKey.label" :placeholder="t('keys.labelPlaceholder')" style="max-width: 320px" required />
      <button class="primary" type="submit" :disabled="creating">
        {{ creating ? t('keys.creating') : t('keys.create') }}
      </button>
    </form>
    <p class="hint">
      {{ t('keys.issueHint') }}
    </p>
  </div>

  <div class="card">
    <div v-if="loading" class="empty">{{ t('common.loading') }}</div>
    <table v-else-if="keys.length">
      <thead>
        <tr>
          <th>{{ t('keys.colKey') }}</th>
          <th>{{ t('keys.colLabel') }}</th>
          <th>{{ t('keys.colStatus') }}</th>
          <th>{{ t('keys.colBuckets') }}</th>
          <th>{{ t('keys.colLastUsed') }}</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <template v-for="k in keys" :key="k.access_key_id">
          <tr>
            <td class="mono">{{ k.access_key_id }}</td>
            <td>{{ k.label }}</td>
            <td>
              <span class="badge" :class="k.enabled ? 'ok' : 'danger'">
                {{ k.enabled ? t('keys.enabled') : t('keys.disabled') }}
              </span>
            </td>
            <td>
              <div class="row" v-if="(k.grants || []).length">
                <span v-for="g in k.grants" :key="g.bucket" class="badge">
                  {{ g.bucket }} · {{ g.read ? t('keys.read') : '' }}{{ g.write ? t('keys.write') : '' }}
                </span>
              </div>
              <span v-else class="muted">{{ t('keys.none') }}</span>
            </td>
            <td class="muted">{{ ago(k.last_used_at) }}</td>
            <td class="row end">
              <button class="small" @click="startEdit(k)">{{ t('keys.grants') }}</button>
              <button class="small ghost" @click="toggle(k)">{{ k.enabled ? t('keys.disable') : t('keys.enable') }}</button>
              <button class="small ghost" @click="rotate(k)">{{ t('keys.rotate') }}</button>
              <button class="small danger" @click="remove(k)">{{ t('keys.delete') }}</button>
            </td>
          </tr>
          <tr v-if="editing === k.access_key_id">
            <td colspan="6" style="background: var(--bg)">
              <div class="card" style="margin: 6px 0; background: var(--bg-elev-2)">
                <h3 style="margin-bottom: 12px">{{ t('keys.grantsFor', { label: k.label }) }}</h3>
                <div v-if="buckets.length" class="grid cols-2">
                  <div v-for="b in buckets" :key="b.name" class="row">
                    <span class="mono" style="min-width: 150px">{{ b.name }}</span>
                    <label class="row" style="gap: 5px">
                      <input type="checkbox" v-model="draftGrants[b.name].read" style="width: auto" />
                      {{ t('keys.read') }}
                    </label>
                    <label class="row" style="gap: 5px">
                      <input type="checkbox" v-model="draftGrants[b.name].write" style="width: auto" />
                      {{ t('keys.write') }}
                    </label>
                  </div>
                </div>
                <div v-else class="muted">{{ t('keys.createBucketFirst') }}</div>
                <div class="row end" style="margin-top: 14px">
                  <button class="small ghost" @click="editing = null">{{ t('keys.cancel') }}</button>
                  <button class="small primary" @click="saveGrants(k)">{{ t('keys.saveGrants') }}</button>
                </div>
              </div>
            </td>
          </tr>
        </template>
      </tbody>
    </table>
    <div v-else class="empty">{{ t('keys.empty') }}</div>
  </div>

  <div v-if="issued" class="modal-backdrop" @click.self="issued = null">
    <div class="modal">
      <h2 style="margin-bottom: 6px">{{ issuedTitle }}</h2>
      <p class="dim" style="margin-top: 0">
        {{ t('keys.secretOnce') }}
      </p>
      <label class="field">
        <span>{{ t('keys.accessKeyId') }}</span>
        <input :value="issued.access_key_id" readonly class="mono" />
      </label>
      <label class="field">
        <span>{{ t('keys.secretAccessKey') }}</span>
        <input :value="issued.secret_access_key" readonly class="mono" />
      </label>
      <div class="row end" style="margin-top: 8px">
        <button class="small ghost" @click="doCopy(`AWS_ACCESS_KEY_ID=${issued.access_key_id}\nAWS_SECRET_ACCESS_KEY=${issued.secret_access_key}`)">
          {{ t('keys.copyBoth') }}
        </button>
        <button class="primary small" @click="issued = null">{{ t('keys.stored') }}</button>
      </div>
    </div>
  </div>
</template>
