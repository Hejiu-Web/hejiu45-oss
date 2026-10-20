<script setup>
import { onMounted, ref } from 'vue'
import { api, toast } from '../store.js'
import { ago, bytes } from '../format.js'
import { t } from '../i18n.js'

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
    toast(t('buckets.created', { name: form.value.name }), 'ok')
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
  if (!confirm(t('buckets.confirmDelete', { name: b.name }))) return
  try {
    await api.del(`/api/buckets/${encodeURIComponent(b.name)}`)
    toast(t('buckets.deleted', { name: b.name }), 'ok')
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
      <h1>{{ t('buckets.title') }}</h1>
      <p>{{ t('buckets.subtitle') }}</p>
    </div>
    <button class="primary" @click="showCreate = true">{{ t('buckets.new') }}</button>
  </div>

  <div v-if="error" class="alert error">{{ error }}</div>

  <div class="card">
    <div v-if="loading" class="empty">{{ t('common.loading') }}</div>
    <table v-else-if="buckets.length">
      <thead>
        <tr>
          <th>{{ t('buckets.colBucket') }}</th>
          <th>{{ t('buckets.colVisibility') }}</th>
          <th class="num">{{ t('buckets.colObjects') }}</th>
          <th class="num">{{ t('buckets.colSize') }}</th>
          <th class="num">{{ t('buckets.colKeys') }}</th>
          <th>{{ t('buckets.colCreated') }}</th>
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
              {{ t('buckets.visibility.' + b.visibility) }}
            </span>
          </td>
          <td class="num">{{ b.object_count }}</td>
          <td class="num">{{ bytes(b.total_bytes) }}</td>
          <td class="num">{{ grantCount(b) }}</td>
          <td class="muted">{{ ago(b.created_at) }}</td>
          <td class="row end">
            <router-link class="btn small" :to="`/buckets/${encodeURIComponent(b.name)}`">
              {{ t('buckets.browse') }}
            </router-link>
            <button class="small danger" @click="remove(b)">{{ t('buckets.delete') }}</button>
          </td>
        </tr>
      </tbody>
    </table>
    <div v-else class="empty">
      {{ t('buckets.empty') }}
      <button class="small primary" style="margin-left: 8px" @click="showCreate = true">
        {{ t('buckets.createFirst') }}
      </button>
    </div>
  </div>

  <div v-if="showCreate" class="modal-backdrop" @click.self="showCreate = false">
    <form class="modal" @submit.prevent="create">
      <h2 style="margin-bottom: 16px">{{ t('buckets.new') }}</h2>
      <label class="field">
        <span>{{ t('buckets.name') }}</span>
        <input
          v-model="form.name"
          placeholder="my-bucket"
          pattern="[a-z0-9][a-z0-9.\-]{1,61}[a-z0-9]"
          required
          autofocus
        />
        <div class="hint">
          {{ t('buckets.nameHint') }}
        </div>
      </label>
      <label class="field">
        <span>{{ t('buckets.visibility') }}</span>
        <select v-model="form.visibility">
          <option value="private">{{ t('buckets.private') }}</option>
          <option value="public-read">{{ t('buckets.publicRead') }}</option>
        </select>
      </label>
      <div class="row end">
        <button type="button" class="ghost" @click="showCreate = false">{{ t('buckets.cancel') }}</button>
        <button type="submit" class="primary" :disabled="creating">
          {{ creating ? t('buckets.creating') : t('buckets.create') }}
        </button>
      </div>
    </form>
  </div>
</template>
