<script setup>
import { onMounted, ref } from 'vue'
import { api, state } from '../store.js'
import { ago, bytes, duration } from '../format.js'

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

const snippets = [
  {
    label: 'AWS CLI',
    body: `aws --endpoint-url ${'<server>'} s3 ls`,
  },
]
</script>

<template>
  <div class="page-head">
    <div>
      <h1>Overview</h1>
      <p>Storage, credentials and the latest security-relevant events.</p>
    </div>
  </div>

  <div v-if="error" class="alert error">{{ error }}</div>

  <div class="grid cols-4" v-if="data">
    <div class="stat">
      <div class="stat-label">Buckets</div>
      <div class="stat-value">{{ data.buckets }}</div>
    </div>
    <div class="stat">
      <div class="stat-label">Objects</div>
      <div class="stat-value">{{ data.objects }}</div>
    </div>
    <div class="stat">
      <div class="stat-label">Stored</div>
      <div class="stat-value">{{ bytes(data.total_bytes) }}</div>
    </div>
    <div class="stat">
      <div class="stat-label">Access keys</div>
      <div class="stat-value">{{ data.enabled_keys }}<span class="muted" style="font-size: 14px"> / {{ data.keys }}</span></div>
    </div>
  </div>

  <div class="card" v-if="data" style="margin-top: 16px">
    <div class="card-head">
      <h2>Connect a client</h2>
      <span class="badge accent">S3 · SigV4</span>
    </div>
    <p class="dim" style="margin-top: 0">
      hejiu45-oss speaks the S3 API with path-style addressing. Create an access key and
      grant it a bucket, then use the endpoint below.
    </p>
    <pre class="code">aws configure set default.s3.addressing_style path

# List buckets
aws --endpoint-url {{ endpoint() }} s3 ls

# Upload / download
aws --endpoint-url {{ endpoint() }} s3 cp ./file.txt s3://my-bucket/file.txt
aws --endpoint-url {{ endpoint() }} s3 cp s3://my-bucket/file.txt ./file.txt

# Other SDKs: endpoint = {{ endpoint() }}, region = {{ data.region }}, path-style = true</pre>
    <p class="hint">
      The endpoint uses HTTPS with a valid certificate when a reverse proxy terminates TLS
      (see <code>deploy/nginx/hejiu45-oss.conf</code>).
    </p>
  </div>

  <div class="card" v-if="data">
    <div class="card-head">
      <h2>Server</h2>
      <span class="badge ok">running</span>
    </div>
    <div class="grid cols-2">
      <div>
        <div class="stat-label">Data directory</div>
        <div class="mono" style="word-break: break-all">{{ data.data_dir }}</div>
      </div>
      <div>
        <div class="stat-label">Region</div>
        <div class="mono">{{ data.region }}</div>
      </div>
      <div>
        <div class="stat-label">Uptime</div>
        <div>{{ duration(data.uptime_seconds) }}</div>
      </div>
      <div>
        <div class="stat-label">Object types</div>
        <div class="row" v-if="Object.keys(data.by_type).length">
          <span v-for="(n, t) in data.by_type" :key="t" class="badge">{{ t }} · {{ n }}</span>
        </div>
        <div v-else class="muted">no objects yet</div>
      </div>
    </div>
  </div>

  <div class="card">
    <div class="card-head">
      <h2>Recent activity</h2>
      <router-link to="/audit">View all →</router-link>
    </div>
    <table v-if="recent.length">
      <thead>
        <tr>
          <th>When</th>
          <th>Actor</th>
          <th>Action</th>
          <th>Target</th>
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
    <div v-else class="empty">No activity recorded yet.</div>
  </div>
</template>

