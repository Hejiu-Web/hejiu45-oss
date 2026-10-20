<script setup>
import { computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, state } from './store.js'

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

      <router-link class="nav-link" to="/">Overview</router-link>
      <router-link class="nav-link" to="/buckets">Buckets</router-link>
      <router-link class="nav-link" to="/keys">Access keys</router-link>
      <router-link class="nav-link" to="/audit">Audit log</router-link>
      <router-link class="nav-link" to="/settings">Settings</router-link>

      <div class="nav-spacer"></div>
      <div class="dim" style="padding: 8px 10px; font-size: 12px">
        signed in as <strong>{{ state.user?.username }}</strong>
      </div>
      <button class="ghost" @click="signOut">Sign out</button>
    </aside>

    <main class="main">
      <router-view />
    </main>
  </div>

  <div class="toast-host">
    <div v-for="t in state.toasts" :key="t.id" class="toast" :class="t.kind">
      {{ t.message }}
    </div>
  </div>
</template>
