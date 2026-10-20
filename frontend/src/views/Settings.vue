<script setup>
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { api, state, toast } from '../store.js'
import { t } from '../i18n.js'

const origin = window.location.origin

const router = useRouter()
const current = ref('')
const next = ref('')
const repeat = ref('')
const busy = ref(false)
const error = ref('')
const overview = ref(null)

onMounted(async () => {
  try {
    overview.value = await api.get('/api/overview')
  } catch {
    /* non-fatal */
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
