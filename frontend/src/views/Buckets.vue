<script setup>
import { onMounted, ref } from 'vue'
import { api, toast } from '../store.js'
import { ago, bytes } from '../format.js'

const buckets = ref([])
const keys = ref([])
const loading = ref(true)
const showCreate = ref(false)
const creating = ref(false)
const form = ref({ name: '', visibility: 'private' })
const error = ref('')

async function load() {
  loading.value = true
  try {
    const [b, k] = await Promise.all([api.get('/api/buckets'), api.get('/api/keys')])
    buckets.value = b.buckets || []
    keys.value = k.keys || []
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
    await api.post('/api/buckets', form.value)
    toast(`Bucket "${form.value.name}" created.`, 'ok')
    showCreate.value = false
    form.value = { name: '', visibility: 'private' }
    await load()
  } catch (e) {
    error.value = e.message
  } finally {
    creating.value = false
  }
}

async function remove(b) {
  if (!confirm(`Delete bucket "${b.name}"? This cannot be undone.`)) return
  try {
    await api.del(`/api/buckets/${encodeURIComponent(b.name)}`)
    toast(`Bucket "${b.name}" deleted.`, 'ok')
    await load()
  } catch (e) {
    toast(e.message, 'error', 6000)
  }
}

function grantCount(b) {
  return (b.grants || []).length
}

onMounted(load)
</script>

<template>
  <div class="page-head">
    <div>
      <h1>Buckets</h1>
      <p>Every bucket starts private. Access keys only see the buckets they are granted.</p>
    </div>
    <button class="primary" @click="showCreate = true">New bucket</button>
  </div>

  <div v-if="error" class="alert error">{{ error }}</div>

  <div class="card">
    <div v-if="loading" class="empty">Loading…</div>
    <table v-else-if="buckets.length">
      <thead>
        <tr>
          <th>Bucket</th>
          <th>Visibility</th>
          <th class="num">Objects</th>
          <th class="num">Size</th>
          <th class="num">Keys</th>
          <th>Created</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="b in buckets" :key="b.name">
          <td>
            <router-link :to="`/buckets/${encodeURIComponent(b.name)}`" class="mono">
              {{ b.name }}
            </router-link>
          </td>
          <td>
            <span class="badge" :class="b.visibility === 'public-read' ? 'warn' : 'ok'">
              {{ b.visibility }}
            </span>
          </td>
          <td class="num">{{ b.object_count }}</td>
          <td class="num">{{ bytes(b.total_bytes) }}</td>
          <td class="num">{{ grantCount(b) }}</td>
          <td class="muted">{{ ago(b.created_at) }}</td>
          <td class="row end">
            <router-link class="btn small" :to="`/buckets/${encodeURIComponent(b.name)}`">
              Browse
            </router-link>
            <button class="small danger" @click="remove(b)">Delete</button>
          </td>
        </tr>
      </tbody>
    </table>
    <div v-else class="empty">
      No buckets yet.
      <button class="small primary" style="margin-left: 8px" @click="showCreate = true">
        Create the first one
      </button>
    </div>
  </div>

  <div v-if="showCreate" class="modal-backdrop" @click.self="showCreate = false">
    <form class="modal" @submit.prevent="create">
      <h2 style="margin-bottom: 16px">New bucket</h2>
      <label class="field">
        <span>Name</span>
        <input
          v-model="form.name"
          placeholder="my-bucket"
          pattern="[a-z0-9][a-z0-9.\-]{1,61}[a-z0-9]"
          required
          autofocus
        />
        <div class="hint">
          3–63 characters: lowercase letters, digits, dots and hyphens. Must not look like
          an IP address.
        </div>
      </label>
      <label class="field">
        <span>Visibility</span>
        <select v-model="form.visibility">
          <option value="private">Private — all reads require a signed request (recommended)</option>
          <option value="public-read">Public read — anonymous GET/HEAD allowed</option>
        </select>
      </label>
      <div class="row end">
        <button type="button" class="ghost" @click="showCreate = false">Cancel</button>
        <button type="submit" class="primary" :disabled="creating">
          {{ creating ? 'Creating…' : 'Create bucket' }}
        </button>
      </div>
    </form>
  </div>
</template>
