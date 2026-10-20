<script setup>
import { onMounted, ref } from 'vue'
import { api, state } from '../store.js'
import { ago, bytes, duration } from '../format.js'
import { t } from '../i18n.js'

const data = ref(null)
const recent = ref([])
const error = ref('')

onMounted(async () => {
  try {
    data.value = await api.get('/api/overview')
    const audit = await api.get('/api/audit')
    recent.value = (audit.entries || []).slice(0, 8)
  } catch (e) {
    error.value = e.message
  }
})

function endpoint() {
  return state.info?.public_url || window.location.origin
}
</script>

<template>
  <div class="page-head">
    <div>
      <h1>{{ t('overview.title') }}</h1>
      <p>{{ t('overview.subtitle') }}</p>
    </div>
  </div>

  <div v-if="error" class="alert error">{{ error }}</div>

  <div class="grid cols-4" v-if="data">
    <div class="stat">
      <div class="stat-label">{{ t('overview.buckets') }}</div>
      <div class="stat-value">{{ data.buckets }}</div>
    </div>
    <div class="stat">
      <div class="stat-label">{{ t('overview.objects') }}</div>
      <div class="stat-value">{{ data.objects }}</div>
    </div>
    <div class="stat">
      <div class="stat-label">{{ t('overview.stored') }}</div>
      <div class="stat-value">{{ bytes(data.total_bytes) }}</div>
    </div>
    <div class="stat">
      <div class="stat-label">{{ t('overview.keys') }}</div>
      <div class="stat-value">{{ data.enabled_keys }}<span class="muted" style="font-size: 14px"> / {{ data.keys }}</span></div>
    </div>
  </div>

  <div class="card" v-if="data" style="margin-top: 16px">
    <div class="card-head">
      <h2>{{ t('overview.connectTitle') }}</h2>
      <span class="badge accent">S3 · SigV4</span>
    </div>
    <p class="dim" style="margin-top: 0">
      {{ t('overview.connectLead') }}
    </p>
    <pre class="code">aws configure set default.s3.addressing_style path

{{ t('overview.snippetList') }}
aws --endpoint-url {{ endpoint() }} s3 ls

{{ t('overview.snippetTransfer') }}
aws --endpoint-url {{ endpoint() }} s3 cp ./file.txt s3://my-bucket/file.txt
aws --endpoint-url {{ endpoint() }} s3 cp s3://my-bucket/file.txt ./file.txt

{{ t('overview.snippetSdk') }}: endpoint = {{ endpoint() }}, region = {{ data.region }}, path-style = true</pre>
    <p class="hint">
      {{ t('overview.tlsNote', { file: 'deploy/nginx/hejiu45-oss.conf' }) }}
    </p>
  </div>

  <div class="card" v-if="data">
    <div class="card-head">
      <h2>{{ t('overview.server') }}</h2>
      <span class="badge ok">{{ t('overview.running') }}</span>
    </div>
    <div class="grid cols-2">
      <div>
        <div class="stat-label">{{ t('overview.dataDir') }}</div>
        <div class="mono" style="word-break: break-all">{{ data.data_dir }}</div>
      </div>
      <div>
        <div class="stat-label">{{ t('overview.region') }}</div>
        <div class="mono">{{ data.region }}</div>
      </div>
      <div>
        <div class="stat-label">{{ t('overview.uptime') }}</div>
        <div>{{ duration(data.uptime_seconds) }}</div>
      </div>
      <div>
        <div class="stat-label">{{ t('overview.objectTypes') }}</div>
        <div class="row" v-if="Object.keys(data.by_type).length">
          <span v-for="(n, ty) in data.by_type" :key="ty" class="badge">{{ ty }} · {{ n }}</span>
        </div>
        <div v-else class="muted">{{ t('overview.noObjects') }}</div>
      </div>
    </div>
  </div>

  <div class="card">
    <div class="card-head">
      <h2>{{ t('overview.recent') }}</h2>
      <router-link to="/audit">{{ t('overview.viewAll') }} →</router-link>
    </div>
    <table v-if="recent.length">
      <thead>
        <tr>
          <th>{{ t('overview.colWhen') }}</th>
          <th>{{ t('overview.colActor') }}</th>
          <th>{{ t('overview.colAction') }}</th>
          <th>{{ t('overview.colTarget') }}</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="(e, i) in recent" :key="i">
          <td class="muted" :title="e.at">{{ ago(new Date(e.at * 1000).toISOString()) }}</td>
          <td class="mono">{{ e.actor }}</td>
          <td><span class="badge">{{ e.action }}</span></td>
          <td class="mono truncate">{{ e.target }}</td>
        </tr>
      </tbody>
    </table>
    <div v-else class="empty">{{ t('overview.noActivity') }}</div>
  </div>
</template>
