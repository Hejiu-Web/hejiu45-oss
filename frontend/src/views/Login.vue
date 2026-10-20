<script setup>
import { ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, state } from '../store.js'

// location is not exposed to templates; capture it explicitly.
const origin = window.location.origin

const router = useRouter()
const route = useRoute()
const username = ref('admin')
const password = ref('')
const busy = ref(false)
const error = ref('')

async function submit() {
  if (busy.value) return
  busy.value = true
  error.value = ''
  try {
    await api.login(username.value.trim(), password.value)
    password.value = ''
    router.push(route.query.next || '/')
  } catch (e) {
    error.value = e.message || 'Login failed'
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <form class="login-card" @submit.prevent="submit">
    <div class="brand">
      <div class="brand-mark">HJ</div>
      <div>
        <div class="brand-name">hejiu45-oss</div>
        <div class="brand-ver">object storage console</div>
      </div>
    </div>

    <div v-if="error" class="alert error">{{ error }}</div>

    <label class="field">
      <span>Username</span>
      <input v-model="username" autocomplete="username" required />
    </label>
    <label class="field">
      <span>Password</span>
      <input
        v-model="password"
        type="password"
        autocomplete="current-password"
        required
        autofocus
      />
    </label>

    <button class="primary" style="width: 100%; justify-content: center" :disabled="busy">
      {{ busy ? 'Signing in…' : 'Sign in' }}
    </button>

    <p v-if="state.info && !state.info.secure_cookies" class="alert warn" style="margin-top: 16px">
      This connection is not marked secure, so the session cookie is sent without the
      <code>Secure</code> attribute. Put the console behind HTTPS before using it over a
      network.
    </p>
    <p class="hint" style="margin-top: 14px">
      Region <span class="mono">{{ state.info?.region || 'us-east-1' }}</span>
      &middot; server
      <span class="mono">{{ state.info?.public_url || origin }}</span>
    </p>
  </form>
</template>

