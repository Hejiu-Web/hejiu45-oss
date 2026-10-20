<script setup>
import { computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, state } from './store.js'
import { LOCALES, locale, setLocale, t } from './i18n.js'

const route = useRoute()
const router = useRouter()

const isLogin = computed(() => route.path === '/login')

async function signOut() {
  await api.logout()
  router.push('/login')
}
</script>

<template>
  <div v-if="isLogin" class="login-wrap">
    <router-view />
  </div>

  <div v-else class="shell">
    <aside class="sidebar">
      <div class="brand">
        <div class="brand-mark">HJ</div>
        <div>
          <div class="brand-name">hejiu45-oss</div>
          <div class="brand-ver">v{{ state.info?.version || '0.1.0' }}</div>
        </div>
      </div>

      <router-link class="nav-link" to="/">{{ t('nav.overview') }}</router-link>
      <router-link class="nav-link" to="/buckets">{{ t('nav.buckets') }}</router-link>
      <router-link class="nav-link" to="/keys">{{ t('nav.keys') }}</router-link>
      <router-link class="nav-link" to="/audit">{{ t('nav.audit') }}</router-link>
      <router-link class="nav-link" to="/settings">{{ t('nav.settings') }}</router-link>

      <div class="nav-spacer"></div>

      <div class="lang-switch" role="group" :aria-label="t('lang.switch')">
        <button
          v-for="l in LOCALES"
          :key="l.code"
          class="lang-btn"
          :class="{ active: locale === l.code }"
          type="button"
          :aria-pressed="locale === l.code"
          @click="setLocale(l.code)"
        >
          {{ l.short }}
        </button>
      </div>

      <div class="dim" style="padding: 8px 10px; font-size: 12px">
        {{ t('app.signedInAs') }} <strong>{{ state.user?.username }}</strong>
      </div>
      <button class="ghost" @click="signOut">{{ t('app.signOut') }}</button>
    </aside>

    <main class="main">
      <router-view />
    </main>
  </div>

  <div class="toast-host">
    <div v-for="tst in state.toasts" :key="tst.id" class="toast" :class="tst.kind">
      {{ tst.message }}
    </div>
  </div>
</template>
