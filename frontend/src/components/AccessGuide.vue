<script setup>
// Access guide: copy-paste credentials and snippets for the common SDKs, plus
// a downloadable pre-filled config. Shown per bucket so the endpoint, bucket
// name and region are already correct.
import { computed, ref } from 'vue'
import { t } from '../i18n.js'
import { copy } from '../format.js'

const props = defineProps({
  bucket: { type: String, required: true },
  endpoint: { type: String, required: true },
  region: { type: String, default: 'us-east-1' },
  /** Keys that can read this bucket; the first is preselected. */
  keys: { type: Array, default: () => [] },
  /** Full public URL prefix, empty when the bucket is private. */
  publicPrefix: { type: String, default: '' },
  publicUrl: { type: String, default: '' },
  accessKeyId: { type: String, default: '' },
  secretKey: { type: String, default: '' },
})

const tab = ref('awscli')
const picked = ref(props.accessKeyId || props.keys[0]?.access_key_id || '')

const tabs = computed(() => [
  { id: 'awscli', label: 'aws-cli' },
  { id: 'boto3', label: 'Python (boto3)' },
  { id: 'js', label: 'JavaScript' },
  { id: 'go', label: 'Go' },
])

const keyId = computed(() => picked.value || 'YOUR_ACCESS_KEY_ID')
const secret = computed(() => props.secretKey || 'YOUR_SECRET_ACCESS_KEY')

const snippets = computed(() => {
  const ep = props.endpoint
  const b = props.bucket
  const ak = keyId.value
  const sk = secret.value
  const rg = props.region
  return {
    awscli: `aws configure set default.s3.addressing_style path
export AWS_ACCESS_KEY_ID=${ak}
export AWS_SECRET_ACCESS_KEY=${sk}

aws --endpoint-url ${ep} s3 ls s3://${b}/
aws --endpoint-url ${ep} s3 cp ./file.txt s3://${b}/file.txt
aws --endpoint-url ${ep} s3 cp s3://${b}/file.txt ./file.txt
aws --endpoint-url ${ep} s3 sync ./dist s3://${b}/dist/`,
    boto3: `import boto3

s3 = boto3.client(
    "s3",
    endpoint_url="${ep}",
    aws_access_key_id="${ak}",
    aws_secret_access_key="${sk}",
    region_name="${rg}",
    config=boto3.session.Config(s3={"addressing_style": "path"}),
)

s3.upload_file("file.txt", "${b}", "file.txt")
s3.download_file("${b}", "file.txt", "download.txt")
for obj in s3.list_objects_v2(Bucket="${b}").get("Contents", []):
    print(obj["Key"], obj["Size"])`,
    js: `import { S3Client, PutObjectCommand, ListObjectsV2Command } from "@aws-sdk/client-s3";

const s3 = new S3Client({
  endpoint: "${ep}",
  region: "${rg}",
  forcePathStyle: true,       // required: path-style addressing
  credentials: {
    accessKeyId: "${ak}",
    secretAccessKey: "${sk}",
  },
});

await s3.send(new PutObjectCommand({
  Bucket: "${b}",
  Key: "hello.txt",
  Body: "hello from JS",
}));

const list = await s3.send(new ListObjectsV2Command({ Bucket: "${b}" }));
console.log(list.Contents);`,
    go: `package main

import (
	"context"
	"log"
	"strings"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/credentials"
	"github.com/aws/aws-sdk-go-v2/service/s3"
)

func main() {
	client := s3.New(s3.Options{
		BaseEndpoint: aws.String("${ep}"),
		Region:       "${rg}",
		Credentials:  credentials.NewStaticCredentialsProvider("${ak}", "${sk}", ""),
		UsePathStyle: true,
	})

	out, err := client.ListObjectsV2(context.TODO(), &s3.ListObjectsV2Input{
		Bucket: aws.String("${b}"),
	})
	if err != nil {
		log.Fatal(err)
	}
	for _, o := range out.Contents {
		log.Println(*o.Key, *o.Size)
	}
}`,
  }
})

const current = computed(() => snippets.value[tab.value] || '')

async function copyCurrent() {
  if (await copy(current.value)) flash(t('keys.copied'))
}

/** Download a ready-to-use ~/.aws config + credentials pair. */
function downloadAwsConfig() {
  const cfg = `# ~/.aws/config
[default]
region = ${props.region}
s3 =
    addressing_style = path
    endpoint_url = ${props.endpoint}
`
  const cred = `# ~/.aws/credentials
[default]
aws_access_key_id = ${keyId.value}
aws_secret_access_key = ${secret.value}
`
  download(`aws-config-${props.bucket}.txt`, `=== ~/.aws/config ===\n${cfg}\n=== ~/.aws/credentials ===\n${cred}`)
}

/** Download a .env file for application code. */
function downloadEnv() {
  const body = `# hejiu45-oss — bucket "${props.bucket}"
S3_ENDPOINT=${props.endpoint}
S3_REGION=${props.region}
S3_BUCKET=${props.bucket}
S3_ACCESS_KEY_ID=${keyId.value}
S3_SECRET_ACCESS_KEY=${secret.value}
S3_FORCE_PATH_STYLE=true
`
  download(`hejiu45-${props.bucket}.env`, body)
}

function download(name, text) {
  const blob = new Blob([text], { type: 'text/plain;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = name
  a.click()
  URL.revokeObjectURL(url)
}

const msg = ref('')
let timer = null
function flash(text) {
  msg.value = text
  clearTimeout(timer)
  timer = setTimeout(() => (msg.value = ''), 2200)
}

async function copyText(text) {
  if (await copy(text)) flash(t('keys.copied'))
}
</script>

<template>
  <div class="card">
    <div class="card-head">
      <h2>{{ t('guide.title') }}</h2>
      <span class="badge accent">{{ t('guide.badge') }}</span>
    </div>
    <p class="dim" style="margin-top: 0">{{ t('guide.lead') }}</p>

    <div v-if="keys.length" class="guide-key-picker">
      <label class="field" style="margin: 0; max-width: 320px">
        <span>{{ t('guide.whichKey') }}</span>
        <select v-model="picked">
          <option v-for="k in keys" :key="k.access_key_id" :value="k.access_key_id">
            {{ k.label }} ({{ k.access_key_id }})
          </option>
        </select>
      </label>
      <p class="hint" style="margin: 6px 0 0">{{ t('guide.secretHint') }}</p>
    </div>

    <div class="row" style="gap: 8px; flex-wrap: wrap; margin: 12px 0">
      <button
        v-for="x in tabs"
        :key="x.id"
        class="small"
        :class="tab === x.id ? 'primary' : 'ghost'"
        type="button"
        @click="tab = x.id"
      >
        {{ x.label }}
      </button>
    </div>

    <pre class="code">{{ current }}</pre>
    <div class="row" style="gap: 8px; margin-top: 10px">
      <button class="small" type="button" @click="copyCurrent">{{ t('guide.copySnippet') }}</button>
      <button class="small ghost" type="button" @click="downloadAwsConfig">{{ t('guide.downloadAws') }}</button>
      <button class="small ghost" type="button" @click="downloadEnv">{{ t('guide.downloadEnv') }}</button>
      <span v-if="msg" class="hint">{{ msg }}</span>
    </div>

    <div class="guide-endpoints">
      <div>
        <div class="stat-label">S3 endpoint</div>
        <code class="mono">{{ endpoint }}</code>
      </div>
      <div>
        <div class="stat-label">{{ t('guide.region') }}</div>
        <code class="mono">{{ region }}</code>
      </div>
      <div>
        <div class="stat-label">{{ t('guide.pathStyle') }}</div>
        <code class="mono">true</code>
      </div>
      <div v-if="publicPrefix">
        <div class="stat-label">{{ t('guide.publicPrefix') }}</div>
        <code class="mono">{{ publicPrefix }}</code>
        <button class="small ghost" type="button" style="margin-left: 8px" @click="copyText(publicPrefix)">
          {{ t('detail.copyUrl') }}
        </button>
      </div>
    </div>

    <p v-if="publicUrl" class="hint">
      {{ t('guide.oneFileExample') }}
      <code class="mono">{{ publicUrl }}</code>
    </p>
  </div>
</template>
