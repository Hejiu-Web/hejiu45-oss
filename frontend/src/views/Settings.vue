<script setup>
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { api, state, toast } from '../store.js'

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
    error.value = 'The new passwords do not match.'
    return
  }
  if (next.value.length < 12) {
    error.value = 'Use at least 12 characters.'
    return
  }
  busy.value = true
  try {
    await api.post('/api/auth/password', {
      current_password: current.value,
      new_password: next.value,
    })
    toast('Password changed. Every session was revoked — sign in again.', 'ok', 7000)
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
      <h1>Settings</h1>
      <p>Console account and the server's security posture.</p>
    </div>
  </div>

  <div class="card">
    <div class="card-head"><h2>Security posture</h2></div>
    <div class="grid cols-2">
      <div>
        <div class="stat-label">Secure cookies</div>
        <span class="badge" :class="state.info?.secure_cookies ? 'ok' : 'danger'">
          {{ state.info?.secure_cookies ? 'enabled' : 'disabled' }}
        </span>
        <div class="hint">
          Requires TLS in-process (<code>--tls manual</code>) or a trusted reverse proxy
          (<code>--tls reverse-proxy</code>) reporting <code>X-Forwarded-Proto: https</code>.
        </div>
      </div>
      <div>
        <div class="stat-label">Console bundle</div>
        <span class="badge" :class="state.info?.console_embedded ? 'ok' : 'warn'">
          {{ state.info?.console_embedded ? 'embedded' : 'placeholder' }}
        </span>
        <div class="hint">The whole console ships inside the binary; no files needed at runtime.</div>
      </div>
      <div>
        <div class="stat-label">Region</div>
        <div class="mono">{{ state.info?.region }}</div>
      </div>
      <div>
        <div class="stat-label">Public endpoint</div>
        <div class="mono">{{ state.info?.public_url || origin }}</div>
      </div>
      <div v-if="overview">
        <div class="stat-label">Data directory</div>
        <div class="mono" style="word-break: break-all">{{ overview.data_dir }}</div>
      </div>
    </div>
  </div>

  <div class="card">
    <div class="card-head"><h2>Change password</h2></div>
    <div v-if="error" class="alert error">{{ error }}</div>
    <form style="max-width: 420px" @submit.prevent="changePassword">
      <label class="field">
        <span>Current password</span>
        <input v-model="current" type="password" autocomplete="current-password" required />
      </label>
      <label class="field">
        <span>New password (min. 12 characters)</span>
        <input v-model="next" type="password" autocomplete="new-password" required minlength="12" />
      </label>
      <label class="field">
        <span>Repeat new password</span>
        <input v-model="repeat" type="password" autocomplete="new-password" required minlength="12" />
      </label>
      <button class="primary" type="submit" :disabled="busy">
        {{ busy ? 'Updating…' : 'Change password' }}
      </button>
      <p class="hint">Changing the password signs out every device, including this one.</p>
    </form>
  </div>

  <div class="card">
    <div class="card-head"><h2>Backups</h2></div>
    <p class="dim" style="margin-top: 0">
      Metadata and object bodies live under the data directory. Take a consistent copy
      offline with the built-in command:
    </p>
    <pre class="code">hejiu45-oss --data-dir&nbsp;{{ overview?.data_dir || '/var/lib/hejiu45-oss' }} \
  --backup-to /backups/meta-$(date +%F).redb

# then copy the objects/ tree alongside it
tar -C {{ overview?.data_dir || '/var/lib/hejiu45-oss' }} -czf /backups/objects-$(date +%F).tgz objects</pre>
    <p class="hint">
      Restore with <code>--restore-from</code> into an empty data directory. The master key
      (<code>master.key</code> or <code>HEJIU45_OSS_MASTER_KEY</code>) is required to decrypt
      stored S3 secrets — back it up separately.
    </p>
  </div>
</template>

