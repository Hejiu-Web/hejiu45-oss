<script setup>
import { onMounted, ref } from 'vue'
import { api } from '../store.js'
import { dateTime } from '../format.js'

const entries = ref([])
const error = ref('')
const loading = ref(true)

onMounted(async () => {
  try {
    const r = await api.get('/api/audit')
    entries.value = r.entries || []
  } catch (e) {
    error.value = e.message
  } finally {
    loading.value = false
  }
})

function kind(action) {
  if (action.includes('failed') || action.includes('delete') || action.includes('disable')) return 'danger'
  if (action.includes('grants') || action.includes('rotate') || action.includes('password')) return 'warn'
  return 'accent'
}
</script>

<template>
  <div class="page-head">
    <div>
      <h1>Audit log</h1>
      <p>Newest first, capped at the most recent 200 events. Stored in the metadata database.</p>
    </div>
  </div>

  <div v-if="error" class="alert error">{{ error }}</div>

  <div class="card">
    <div v-if="loading" class="empty">Loading…</div>
    <table v-else-if="entries.length">
      <thead>
        <tr>
          <th>Time (UTC)</th>
          <th>Actor</th>
          <th>Action</th>
          <th>Target</th>
          <th>Detail</th>
          <th>Client</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="(e, i) in entries" :key="i">
          <td class="mono muted">{{ dateTime(new Date(e.at * 1000).toISOString()) }}</td>
          <td class="mono">{{ e.actor }}</td>
          <td><span class="badge" :class="kind(e.action)">{{ e.action }}</span></td>
          <td class="mono truncate">{{ e.target }}</td>
          <td class="muted truncate">{{ e.detail }}</td>
          <td class="mono muted">{{ e.remote_ip }}</td>
        </tr>
      </tbody>
    </table>
    <div v-else class="empty">No events recorded.</div>
  </div>
</template>
