import { createRouter, createWebHashHistory } from 'vue-router'
import { state, api } from './store.js'

// Lazy-loaded views keep the initial bundle small; the whole console is still
// embedded in the binary, so these are local chunks, not network fetches.
const Login = () => import('./views/Login.vue')
const Overview = () => import('./views/Overview.vue')
const Buckets = () => import('./views/Buckets.vue')
const BucketDetail = () => import('./views/BucketDetail.vue')
const Keys = () => import('./views/Keys.vue')
const Audit = () => import('./views/Audit.vue')
const Settings = () => import('./views/Settings.vue')

// Hash history: the server only ever serves `/`, so no SPA fallback routing or
// per-route rewriting is needed and deep links survive being embedded in the
// binary (see `src/web.rs`).
const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: '/login', component: Login, meta: { public: true } },
    { path: '/', component: Overview },
    { path: '/buckets', component: Buckets },
    { path: '/buckets/:name', component: BucketDetail, props: true },
    { path: '/keys', component: Keys },
    { path: '/audit', component: Audit },
    { path: '/settings', component: Settings },
    { path: '/:pathMatch(.*)*', redirect: '/' },
  ],
})

/**
 * Resolve the session once, before the first navigation completes.
 *
 * Doing this inside `beforeEach` and *returning a resolved value* (not just
 * mutating state) is important: when the guard only updates the store, Vue
 * Router has already matched the target and renders an empty view until the
 * next navigation.
 */
let sessionPromise = null
function ensureSession() {
  if (!sessionPromise) {
    sessionPromise = (async () => {
      if (!state.info) {
        state.info = await api.get('/api/info').catch(() => null)
      }
      state.user = await api.get('/api/auth/me').catch(() => null)
      return state.user
    })()
  }
  return sessionPromise
}

/** Called after login/logout so the guard re-resolves the session. */
export function resetSession() {
  sessionPromise = null
}

router.beforeEach(async (to) => {
  const user = await ensureSession()
  if (!to.meta.public && !user) {
    return { path: '/login', query: to.fullPath !== '/' ? { next: to.fullPath } : {} }
  }
  if (to.path === '/login' && user) {
    return { path: '/' }
  }
  return true
})

export default router
