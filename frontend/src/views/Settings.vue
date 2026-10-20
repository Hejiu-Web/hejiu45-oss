<script setup>
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { api, state, toast } from '../store.js'
import { t } from '../i18n.js'
import { bytes } from '../format.js'

const origin = window.location.origin

const router = useRouter()
const current = ref('')
const next = ref('')
const repeat = ref('')
const busy = ref(false)
const error = ref('')
const overview = ref(null)

// --- upload limits -----------------------------------------------------------
const MiB = 1024 * 1024
const limits = ref(null)
const unlimited = ref(false)
const maxMiB = ref(5120)
const partMiB = ref(8)
const savingLimits = ref(false)
const limitsError = ref('')

function loadLimits(doc) {
  limits.value = doc
  unlimited.value = doc.max_object_bytes == null
  maxMiB.value = doc.max_object_bytes ? Math.round(doc.max_object_bytes / MiB) : 5120
  partMiB.value = Math.round(doc.part_bytes / MiB)
}

async function saveLimits() {
  limitsError.value = ''
  savingLimits.value = true
  try {
    const payload = {
      max_object_bytes: unlimited.value ? null : Math.max(1, Math.round(Number(maxMiB.value))) * MiB,
      part_bytes: Math.round(Number(partMiB.value)) * MiB,
    }
    const saved = await api.patch('/api/settings/limits', payload)
    loadLimits(saved)
    toast(t('limits.saved'), 'ok')
  } catch (e) {
    limitsError.value = e.message || t('limits.saveFailed')
  } finally {
    savingLimits.value = false
  }
}

onMounted(async () => {
  try {
    overview.value = await api.get('/api/overview')
  } catch {
    /* non-fatal */
  }
  try {
    loadLimits(await api.get('/api/settings/limits'))
  } catch {
    /* the card stays hidden if the endpoint is unavailable */
  }
})

async function changePassword() {
  error.value = ''
  if (next.value !== repeat.value) {
    error.value = t('settings.passwordMismatch')
    return
  }
  if (next.value.length < 12) {
    error.value = t('settings.passwordTooShort')
    return
  }
  busy.value = true
  try {
    await api.post('/api/auth/password', {
      current_password: current.value,
      new_password: next.value,
    })
    toast(t('settings.passwordChanged'), 'ok', 7000)
    state.user = null
    router.push('/login')
  } catch (e) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="page-head">
    <div>
      <h1>{{ t('settings.title') }}</h1>
      <p>{{ t('settings.subtitle') }}</p>
    </div>
  </div>

  <div class="card">
    <div class="card-head"><h2>{{ t('settings.posture') }}</h2></div>
    <div class="grid cols-2">
      <div>
        <div class="stat-label">{{ t('settings.secureCookies') }}</div>
        <span class="badge" :class="state.info?.secure_cookies ? 'ok' : 'danger'">
          {{ state.info?.secure_cookies ? t('settings.enabled') : t('settings.disabled') }}
        </span>
        <div class="hint">
          {{ t('settings.secureCookiesHint') }}
        </div>
      </div>
      <div>
        <div class="stat-label">{{ t('settings.bundle') }}</div>
        <span class="badge" :class="state.info?.console_embedded ? 'ok' : 'warn'">
          {{ state.info?.console_embedded ? t('settings.embedded') : t('settings.placeholder') }}
        </span>
        <div class="hint">{{ t('settings.bundleHint') }}</div>
      </div>
      <div>
        <div class="stat-label">{{ t('settings.region') }}</div>
        <div class="mono">{{ state.info?.region }}</div>
      </div>
      <div>
        <div class="stat-label">{{ t('settings.publicEndpoint') }}</div>
        <div class="mono">{{ state.info?.public_url || origin }}</div>
      </div>
      <div v-if="overview">
        <div class="stat-label">{{ t('settings.dataDir') }}</div>
        <div class="mono" style="word-break: break-all">{{ overview.data_dir }}</div>
      </div>
    </div>
  </div>

  <div class="card" v-if="limits">
    <div class="card-head"><h2>{{ t('limits.title') }}</h2></div>
    <p class="dim" style="margin-top: 0">{{ t('limits.subtitle') }}</p>
    <div v-if="limitsError" class="alert error">{{ limitsError }}</div>
    <form style="max-width: 520px" @submit.prevent="saveLimits">
      <label class="field">
        <span>{{ t('limits.maxObject') }}</span>
        <div class="row" style="gap: 8px">
          <input
            v-model.number="maxMiB"
            type="number"
            min="1"
            step="1"
            :disabled="unlimited"
            style="max-width: 160px"
          />
          <span class="hint">MiB</span>
          <label class="row" style="gap: 5px">
            <input v-model="unlimited" type="checkbox" style="width: auto" />
            {{ t('limits.unlimited') }}
          </label>
        </div>
        <div class="hint">{{ t('limits.maxObjectHint') }}</div>
      </label>
      <label class="field">
        <span>{{ t('limits.partSize') }}</span>
        <div class="row" style="gap: 8px">
          <input
            v-model.number="partMiB"
            type="number"
            min="5"
            max="512"
            step="1"
            style="max-width: 160px"
          />
          <span class="hint">MiB</span>
        </div>
        <div class="hint">{{ t('limits.partSizeHint') }}</div>
      </label>
      <button class="primary" type="submit" :disabled="savingLimits">
        {{ savingLimits ? t('limits.saving') : t('limits.save') }}
      </button>
      <p class="hint">
        {{
          t('limits.current', {
            max: limits.max_object_bytes
              ? bytes(limits.max_object_bytes)
              : t('limits.unlimitedValue'),
            part: bytes(limits.part_bytes),
          })
        }}
      </p>
    </form>
  </div>

  <div class="card">
    <div class="card-head"><h2>{{ t('settings.changePassword') }}</h2></div>
    <div v-if="error" class="alert error">{{ error }}</div>
    <form style="max-width: 420px" @submit.prevent="changePassword">
      <label class="field">
        <span>{{ t('settings.currentPassword') }}</span>
        <input v-model="current" type="password" autocomplete="current-password" required />
      </label>
      <label class="field">
        <span>{{ t('settings.newPassword') }}</span>
        <input v-model="next" type="password" autocomplete="new-password" required minlength="12" />
      </label>
      <label class="field">
        <span>{{ t('settings.repeatPassword') }}</span>
        <input v-model="repeat" type="password" autocomplete="new-password" required minlength="12" />
      </label>
      <button class="primary" type="submit" :disabled="busy">
        {{ busy ? t('settings.updating') : t('settings.update') }}
      </button>
      <p class="hint">{{ t('settings.passwordHint') }}</p>
    </form>
  </div>

  <div class="card">
    <div class="card-head"><h2>{{ t('settings.backups') }}</h2></div>
    <p class="dim" style="margin-top: 0">
      {{ t('settings.backupsLead') }}
    </p>
    <pre class="code">hejiu45-oss --data-dir&nbsp;{{ overview?.data_dir || '/var/lib/hejiu45-oss' }} \
  --backup-to /backups/meta-$(date +%F).redb

{{ t('settings.backupsThen') }}
tar -C {{ overview?.data_dir || '/var/lib/hejiu45-oss' }} -czf /backups/objects-$(date +%F).tgz objects</pre>
    <p class="hint">
      {{ t('settings.backupsNote') }}
    </p>
  </div>
</template>
