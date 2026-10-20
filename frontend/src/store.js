// Minimal reactive store + API client. Plain JavaScript, no dependencies
// beyond Vue itself.

import { reactive } from 'vue'

export const state = reactive({
  user: null,
  info: null,
  loading: false,
  toasts: [],
})

let toastId = 0
export function toast(message, kind = 'info', ms = 4200) {
  const id = ++toastId
  state.toasts.push({ id, message, kind })
  setTimeout(() => {
    const i = state.toasts.findIndex((t) => t.id === id)
    if (i >= 0) state.toasts.splice(i, 1)
  }, ms)
}

export class ApiError extends Error {
  constructor(status, code, message) {
    super(message)
    this.status = status
    this.code = code
  }
}

/**
 * Fetch wrapper for the console API.
 *
 * On 401 the local user is cleared; the router guard then redirects to the
 * login page. Requests always send credentials (the session is a cookie) and
 * never send a body for GET/DELETE.
 */
async function request(method, path, body) {
  const init = {
    method,
    credentials: 'same-origin',
    headers: { Accept: 'application/json' },
  }
  if (body !== undefined) {
    init.headers['Content-Type'] = 'application/json'
    init.body = JSON.stringify(body)
  }
  let res
  try {
    res = await fetch(path, init)
  } catch (e) {
    throw new ApiError(0, 'NetworkError', 'Cannot reach the server.')
  }
  if (res.status === 401) {
    state.user = null
  }
  const text = await res.text()
  let data = null
  if (text) {
    try {
      data = JSON.parse(text)
    } catch {
      data = { error: { code: 'BadResponse', message: text.slice(0, 200) } }
    }
  }
  if (!res.ok) {
    const err = data && data.error ? data.error : {}
    throw new ApiError(res.status, err.code || 'Error', err.message || res.statusText)
  }
  return data
}

export const api = {
  get: (p) => request('GET', p),
  post: (p, b) => request('POST', p, b ?? {}),
  put: (p, b) => request('PUT', p, b ?? {}),
  patch: (p, b) => request('PATCH', p, b ?? {}),
  del: (p) => request('DELETE', p),

  async loadInfo() {
    state.info = await api.get('/api/info')
    return state.info
  },
  async loadUser() {
    try {
      state.user = await api.get('/api/auth/me')
    } catch {
      state.user = null
    }
    return state.user
  },
  async login(username, password) {
    const { resetSession } = await import('./router.js')
    const r = await api.post('/api/auth/login', { username, password })
    await api.loadUser()
    resetSession()
    return r
  },
  async logout() {
    const { resetSession } = await import('./router.js')
    try {
      await api.post('/api/auth/logout')
    } finally {
      state.user = null
      resetSession()
    }
  },
}

