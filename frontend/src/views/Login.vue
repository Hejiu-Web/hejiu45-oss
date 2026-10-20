<script setup>
import { computed, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, state } from '../store.js'
import { LOCALES, locale, setLocale, t } from '../i18n.js'

// location is not exposed to templates; capture it explicitly.
const origin = window.location.origin

const router = useRouter()
const route = useRoute()
const username = ref('admin')
const password = ref('')
const busy = ref(false)
const error = ref('')

const serverLabel = computed(() => state.info?.public_url || origin)

async function submit() {
  if (busy.value) return
  busy.value = true
  error.value = ''
  try {
    await api.login(username.value.trim(), password.value)
    password.value = ''
    router.push(route.query.next || '/')
  } catch (e) {
    error.value = e.message || t('login.failed')
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="login-lang">
    <button
      v-for="l in LOCALES"
      :key="l.code"
      class="lang-btn"
      :class="{ active: locale === l.code }"
      type="button"
      :aria-pressed="locale === l.code"
      @click="setLocale(l.code)"
    >
      {{ l.label }}
    </button>
  </div>

  <form class="login-card" @submit.prevent="submit">
    <div class="brand">
      <div class="brand-mark">HJ</div>
      <div>
        <div class="brand-name">hejiu45-oss</div>
        <div class="brand-ver">{{ t('app.tagline') }}</div>
      </div>
    </div>

    <div v-if="error" class="alert error">{{ error }}</div>

    <label class="field">
      <span>{{ t('login.username') }}</span>
      <input v-model="username" autocomplete="username" required />
    </label>
    <label class="field">
      <span>{{ t('login.password') }}</span>
      <input
        v-model="password"
        type="password"
        autocomplete="current-password"
        required
        autofocus
      />
    </label>

    <button class="primary" style="width: 100%; justify-content: center" :disabled="busy">
      {{ busy ? t('login.submitting') : t('login.submit') }}
    </button>

    <p v-if="state.info && !state.info.secure_cookies" class="alert warn" style="margin-top: 16px">
      {{ t('login.insecure') }}
    </p>
    <p class="hint" style="margin-top: 14px">
      {{ t('login.region') }} <span class="mono">{{ state.info?.region || 'us-east-1' }}</span>
      &middot; server
      <span class="mono">{{ serverLabel }}</span>
    </p>
  </form>
</template>
