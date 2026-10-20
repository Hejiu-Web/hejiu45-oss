// Upload engine for the console.
//
// Two modes, chosen per upload:
//
//   console — the browser sends bytes to `/api/buckets/{b}/objects` and
//             `/api/.../uploads/...` using the session cookie. No access key
//             needed, which is what an operator expects right after signing in.
//   presigned — the console mints a PUT presigned URL from a chosen access key
//             and the browser PUTs straight to `/s3`. Keeps the "byte paths are
//             scoped by grants" model when that matters.
//
// Both follow the same shape: small files go in one request, large files are
// split at the server-advertised part size and uploaded a few at a time.

import { reactive } from 'vue'
import { api } from './store.js'
import { t } from './i18n.js'

/** Tracks one file from selection to completion. */
export function createJob(file, key, mode, accessKeyId, bucket) {
  return reactive({
    id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    file,
    key,
    bucket,
    name: key.split('/').pop() || key,
    size: file.size,
    mode,
    accessKeyId,
    loaded: 0,
    status: 'pending', // pending | uploading | done | error | cancelled
    error: '',
    speed: 0,
    startedAt: 0,
    uploadId: null,
  })
}

const CONCURRENCY = 3;

/**
 * Upload one job.
 *
 * `partBytes` comes from the server so the browser never guesses a size the
 * server would reject. `signal` aborts mid-flight and the caller is expected to
 * have called `cancelUpload` for cleanup of any open multipart upload.
 */
export async function uploadJob(job, { partBytes, onProgress, signal, maxBytes }) {
  job.status = 'uploading'
  job.startedAt = Date.now()
  job.loaded = 0
  job.error = ''

  // Refuse locally when we already know it cannot fit: it saves the user from
  // watching a 2 GB progress bar fail at 99 %, and the server enforces the same
  // limit anyway.
  if (maxBytes != null && job.size > maxBytes) {
    job.status = 'error'
    job.error = t('upload.tooLarge', { max: formatBytes(maxBytes) })
    throw new Error(job.error)
  }

  try {
    if (job.size <= partBytes) {
      await uploadSingle(job, { onProgress, signal })
    } else {
      await uploadMultipart(job, { partBytes, onProgress, signal })
    }
    job.status = 'done'
    job.loaded = job.size
  } catch (e) {
    if (signal?.aborted) {
      job.status = 'cancelled'
      throw e
    }
    job.status = 'error'
    job.error = e.message || String(e)
    throw e
  }
}

async function uploadSingle(job, { onProgress, signal }) {
  if (job.mode === 'presigned') {
    // Presigned URLs are short-lived and sized by the SDK, so a single PUT is
    // the only sane shape; large files use the console path.
    const signed = await api.post(
      `/api/buckets/${encodeURIComponent(job.bucket)}/objects/presign?key=${encodeURIComponent(job.key)}`,
      { access_key_id: job.accessKeyId, ttl_seconds: 3600, method: 'PUT' },
    )
    await putWithProgress(signed.url, job.file, {
      signal,
      headers: { 'Content-Type': job.file.type || 'application/octet-stream' },
      onProgress: (n) => {
        job.loaded = n
        onProgress?.(job)
      },
    })
    return
  }

  await putWithProgress(
    `/api/buckets/${encodeURIComponent(job.bucket)}/objects?key=${encodeURIComponent(job.key)}`,
    job.file,
    {
      signal,
      headers: {
        // The browser knows the real type (e.g. image/svg+xml, application/pdf);
        // forwarding it means the stored object is served correctly later.
        'Content-Type': job.file.type || 'application/octet-stream',
      },
      onProgress: (n) => {
        job.loaded = n
        onProgress?.(job)
      },
    },
  )
}

async function uploadMultipart(job, { partBytes, onProgress, signal }) {
  const bucket = encodeURIComponent(job.bucket);
  const started = await api.post(
    `/api/buckets/${bucket}/uploads?key=${encodeURIComponent(job.key)}&content_type=${encodeURIComponent(job.file.type || '')}`,
    {},
  );
  job.uploadId = started.upload_id
  const size = partBytes || started.part_bytes

  const total = Math.ceil(job.size / size)
  const parts = new Array(total)
  let next = 0

  const worker = async () => {
    for (;;) {
      if (signal?.aborted) throw new DOMException('aborted', 'AbortError')
      const index = next++
      if (index >= total) return
      const start = index * size
      const end = Math.min(start + size, job.size)
      const blob = job.file.slice(start, end)
      const res = await fetch(
        `/api/buckets/${bucket}/uploads/${encodeURIComponent(job.uploadId)}/parts/${index + 1}`,
        { method: 'PUT', body: blob, credentials: 'same-origin', signal },
      )
      if (!res.ok) {
        let detail = res.statusText
        try {
          const j = await res.json()
          detail = j?.error?.message || detail
        } catch {
          /* keep statusText */
        }
        throw new Error(detail)
      }
      const j = await res.json()
      parts[index] = { part_number: index + 1, etag: j.etag }
      job.loaded = Math.min(job.size, job.loaded + blob.size)
      onProgress?.(job)
    }
  }

  await Promise.all(Array.from({ length: Math.min(CONCURRENCY, total) }, worker))

  await api.post(
    `/api/buckets/${bucket}/uploads/${encodeURIComponent(job.uploadId)}/complete`,
    { parts },
  )
  job.uploadId = null
}

/** XHR instead of fetch: only XHR reports request-body progress. */
function putWithProgress(url, file, { headers = {}, onProgress, signal }) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    xhr.open('PUT', url, true)
    xhr.withCredentials = true
    for (const [k, v] of Object.entries(headers)) xhr.setRequestHeader(k, v)
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) onProgress?.(e.loaded)
    }
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) return resolve()
      let msg = xhr.statusText || `HTTP ${xhr.status}`
      try {
        const j = JSON.parse(xhr.responseText)
        msg = j?.error?.message || msg
      } catch {
        /* keep default */
      }
      reject(new Error(msg))
    }
    xhr.onerror = () => reject(new Error(t('upload.networkError')))
    xhr.onabort = () => reject(new DOMException('aborted', 'AbortError'))
    if (signal) {
      if (signal.aborted) {
        xhr.abort()
        return reject(new DOMException('aborted', 'AbortError'))
      }
      signal.addEventListener('abort', () => xhr.abort(), { once: true })
    }
    xhr.send(file)
  })
}

/** Release server-side state for an interrupted multipart upload. */
export async function cancelUpload(job) {
  if (!job.uploadId) return
  try {
    await api.del(
      `/api/buckets/${encodeURIComponent(job.bucket)}/uploads/${encodeURIComponent(job.uploadId)}`,
    )
  } catch {
    /* the row may already be gone; nothing to recover */
  }
  job.uploadId = null
}

export function formatBytes(n) {
  if (n === 0 || n == null) return '0 B'
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)))
  const v = n / 1024 ** i
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`
}

export function formatSpeed(bytesPerSecond) {
  if (!bytesPerSecond || bytesPerSecond < 1) return '—'
  return `${formatBytes(bytesPerSecond)}/s`
}

export function formatEta(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return '—'
  const s = Math.round(seconds)
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`
}
