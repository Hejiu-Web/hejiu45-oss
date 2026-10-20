// Bilingual UI strings (English / 简体中文).
//
// Deliberately dependency-free: the console ships inside the binary and is
// built with plain JavaScript, so a full i18n framework would cost more bundle
// size than it saves. `t()` is reactive because `locale` lives in a Vue ref.
//
// Adding a language means adding one object to `messages`; every key must exist
// in every language (checked by `npm run check:i18n`).

import { computed, ref } from 'vue'

export const LOCALES = [
  { code: 'en', label: 'English', short: 'EN' },
  { code: 'zh', label: '中文', short: '中' },
]

const STORAGE_KEY = 'hj45.locale'

function detect() {
  try {
    const saved = localStorage.getItem(STORAGE_KEY)
    if (saved && messages[saved]) return saved
  } catch {
    /* private mode / storage disabled */
  }
  // Follow the browser on first visit; anything not Chinese falls back to
  // English, which is the safer default for a technical audience.
  const langs = navigator.languages?.length ? navigator.languages : [navigator.language || 'en']
  for (const l of langs) {
    const code = String(l || '').toLowerCase()
    if (code.startsWith('zh')) return 'zh'
    if (code.startsWith('en')) return 'en'
  }
  return 'en'
}

export const locale = ref(detect())

export function setLocale(code) {
  if (!messages[code]) return
  locale.value = code
  try {
    localStorage.setItem(STORAGE_KEY, code)
  } catch {
    /* non-fatal */
  }
  document.documentElement.lang = code === 'zh' ? 'zh-CN' : 'en'
}

export function toggleLocale() {
  setLocale(locale.value === 'zh' ? 'en' : 'zh')
}

/** Translate a key; `{name}` placeholders are substituted from `params`. */
export function t(key, params) {
  const table = messages[locale.value] || messages.en
  let s = table[key]
  if (s === undefined) s = messages.en[key]
  if (s === undefined) return key
  if (params) {
    s = s.replace(/\{(\w+)\}/g, (m, k) => (params[k] !== undefined ? String(params[k]) : m))
  }
  return s
}

/** Reactive `t` for use in templates: `{{ $t('nav.overview') }}`. */
export const tComputed = computed(() => t)

const messages = {
  en: {
    // --- shared ---
    'common.loading': 'Loading…',

    // --- brand / shell ---
    'app.tagline': 'object storage console',
    'app.signedInAs': 'signed in as',
    'app.signOut': 'Sign out',
    'lang.switch': 'Language',

    // --- login ---
    'login.username': 'Username',
    'login.password': 'Password',
    'login.submit': 'Sign in',
    'login.submitting': 'Signing in…',
    'login.failed': 'Login failed',
    'login.insecure':
      'This connection is not marked secure, so the session cookie is sent without the Secure attribute. Put the console behind HTTPS before using it over a network.',
    'login.region': 'Region',

    // --- nav ---
    'nav.overview': 'Overview',
    'nav.buckets': 'Buckets',
    'nav.keys': 'Access keys',
    'nav.audit': 'Audit log',
    'nav.settings': 'Settings',

    // --- overview ---
    'overview.title': 'Overview',
    'overview.subtitle': 'Storage, credentials and the latest security-relevant events.',
    'overview.buckets': 'Buckets',
    'overview.objects': 'Objects',
    'overview.stored': 'Stored',
    'overview.keys': 'Access keys',
    'overview.connectTitle': 'Connect a client',
    'overview.connectLead':
      'hejiu45-oss speaks the S3 API with path-style addressing. Create an access key and grant it a bucket, then use the endpoint below.',
    'overview.snippetList': '# List buckets',
    'overview.snippetTransfer': '# Upload / download',
    'overview.snippetSdk': '# Other SDKs',
    'overview.tlsNote':
      'The endpoint uses HTTPS with a valid certificate when a reverse proxy terminates TLS (see {file}).',
    'overview.server': 'Server',
    'overview.running': 'running',
    'overview.dataDir': 'Data directory',
    'overview.region': 'Region',
    'overview.uptime': 'Uptime',
    'overview.objectTypes': 'Object types',
    'overview.noObjects': 'no objects yet',
    'overview.recent': 'Recent activity',
    'overview.viewAll': 'View all',
    'overview.noActivity': 'No activity recorded yet.',
    'overview.colWhen': 'When',
    'overview.colActor': 'Actor',
    'overview.colAction': 'Action',
    'overview.colTarget': 'Target',

    // --- buckets ---
    'buckets.title': 'Buckets',
    'buckets.subtitle':
      'Every bucket starts private. Access keys only see the buckets they are granted.',
    'buckets.new': 'New bucket',
    'buckets.colBucket': 'Bucket',
    'buckets.colVisibility': 'Visibility',
    'buckets.colObjects': 'Objects',
    'buckets.colSize': 'Size',
    'buckets.colKeys': 'Keys',
    'buckets.colCreated': 'Created',
    'buckets.browse': 'Browse',
    'buckets.delete': 'Delete',
    'buckets.empty': 'No buckets yet.',
    'buckets.createFirst': 'Create the first one',
    'buckets.name': 'Name',
    'buckets.nameHint':
      '3–63 characters: lowercase letters, digits, dots and hyphens. Must not look like an IP address.',
    'buckets.visibility': 'Visibility',
    'buckets.private': 'Private — all reads require a signed request (recommended)',
    'buckets.publicRead': 'Public read — anonymous GET/HEAD allowed',
    'buckets.cancel': 'Cancel',
    'buckets.create': 'Create bucket',
    'buckets.creating': 'Creating…',
    'buckets.created': 'Bucket "{name}" created.',
    'buckets.deleted': 'Bucket "{name}" deleted.',
    'buckets.confirmDelete': 'Delete bucket "{name}"? This cannot be undone.',
    'buckets.visibility.private': 'private',
    'buckets.visibility.public-read': 'public-read',

    // --- access keys ---
    'keys.title': 'Access keys',
    'keys.subtitle':
      'Credentials for the S3 data plane. Secrets are stored sealed with AES-256-GCM and are shown exactly once.',
    'keys.issue': 'Issue a key',
    'keys.labelPlaceholder': 'label, e.g. ci-deploy',
    'keys.create': 'Create key',
    'keys.creating': 'Creating…',
    'keys.issueHint':
      'A new key has no bucket access until you grant it below — least privilege by default.',
    'keys.colKey': 'Access key',
    'keys.colLabel': 'Label',
    'keys.colStatus': 'Status',
    'keys.colBuckets': 'Buckets',
    'keys.colLastUsed': 'Last used',
    'keys.enabled': 'enabled',
    'keys.disabled': 'disabled',
    'keys.none': 'none',
    'keys.grants': 'Grants',
    'keys.disable': 'Disable',
    'keys.enable': 'Enable',
    'keys.rotate': 'Rotate',
    'keys.delete': 'Delete',
    'keys.empty': 'No access keys yet.',
    'keys.grantsFor': 'Bucket grants for {label}',
    'keys.read': 'read',
    'keys.write': 'write',
    'keys.createBucketFirst': 'Create a bucket first.',
    'keys.cancel': 'Cancel',
    'keys.saveGrants': 'Save grants',
    'keys.grantsSaved': 'Grants saved.',
    'keys.storeSecret': 'Store this secret now',
    'keys.actionComplete': 'Action complete',
    'keys.secretOnce':
      'The secret is never shown again and cannot be recovered — only replaced (rotate).',
    'keys.accessKeyId': 'Access key ID',
    'keys.secretAccessKey': 'Secret access key',
    'keys.copyBoth': 'Copy both',
    'keys.stored': 'I have stored it',
    'keys.rotated': 'Key rotated; bucket grants migrated.',
    'keys.confirmRotate': 'Rotate "{label}"? The old key stops working immediately.',
    'keys.confirmDelete': 'Delete "{label}" and all of its grants?',
    'keys.copied': 'Copied.',
    'keys.copyFailed': 'Copy failed.',

    // --- audit ---
    'audit.title': 'Audit log',
    'audit.subtitle':
      'Newest first, capped at the most recent 200 events. Stored in the metadata database.',
    'audit.colTime': 'Time (UTC)',
    'audit.colActor': 'Actor',
    'audit.colAction': 'Action',
    'audit.colTarget': 'Target',
    'audit.colDetail': 'Detail',
    'audit.colClient': 'Client',
    'audit.empty': 'No events recorded.',

    // --- settings ---
    'settings.title': 'Settings',
    'settings.subtitle': "Console account and the server's security posture.",
    'settings.posture': 'Security posture',
    'settings.secureCookies': 'Secure cookies',
    'settings.enabled': 'enabled',
    'settings.disabled': 'disabled',
    'settings.secureCookiesHint':
      'Requires TLS in-process (--tls manual) or a trusted reverse proxy (--tls reverse-proxy) reporting X-Forwarded-Proto: https.',
    'settings.bundle': 'Console bundle',
    'settings.embedded': 'embedded',
    'settings.placeholder': 'placeholder',
    'settings.bundleHint':
      'The whole console ships inside the binary; no files needed at runtime.',
    'settings.region': 'Region',
    'settings.publicEndpoint': 'Public endpoint',
    'settings.dataDir': 'Data directory',
    'settings.changePassword': 'Change password',
    'settings.currentPassword': 'Current password',
    'settings.newPassword': 'New password (min. 12 characters)',
    'settings.repeatPassword': 'Repeat new password',
    'settings.update': 'Change password',
    'settings.updating': 'Updating…',
    'settings.passwordHint': 'Changing the password signs out every device, including this one.',
    'settings.passwordMismatch': 'The new passwords do not match.',
    'settings.passwordTooShort': 'Use at least 12 characters.',
    'settings.passwordChanged':
      'Password changed. Every session was revoked — sign in again.',
    'settings.backups': 'Backups',
    'settings.backupsLead':
      'Metadata and object bodies live under the data directory. Take a consistent copy offline with the built-in command:',
    'settings.backupsThen': '# then copy the objects/ tree alongside it',
    'settings.backupsNote':
      'Restore with --restore-from into an empty data directory. The master key (master.key or HEJIU45_OSS_MASTER_KEY) is required to decrypt stored S3 secrets — back it up separately.',

    // --- bucket detail ---
    'detail.objects': 'Objects',
    'detail.filterPlaceholder': 'prefix/filter',
    'detail.filter': 'Filter',
    'detail.parent': 'Parent',
    'detail.colKey': 'Key',
    'detail.colType': 'Type',
    'detail.colSize': 'Size',
    'detail.colModified': 'Modified',
    'detail.prefix': 'prefix',
    'detail.share': 'Share',
    'detail.empty':
      'No objects{under}. Upload with the S3 API (aws s3 cp) or drag files — uploads run over SigV4, never the session cookie.',
    'detail.emptyUnder': ' under "{prefix}"',
    'detail.access': 'Access',
    'detail.accessLead':
      'Read and write are separate. A key with no grant cannot even list this bucket.',
    'detail.colAccessKey': 'Access key',
    'detail.colLabel': 'Label',
    'detail.colRead': 'Read',
    'detail.colWrite': 'Write',
    'detail.yes': 'yes',
    'detail.no': 'no',
    'detail.noAccess': 'No key has access yet. Grant one on the',
    'detail.keysPage': 'Access keys',
    'detail.keysPageSuffix': 'page.',
    'detail.shareTitle': 'Share an object',
    'detail.signingKey': 'Signing key (needs read)',
    'detail.choose': '— choose —',
    'detail.lifetime': 'Lifetime',
    'detail.min5': '5 minutes',
    'detail.min15': '15 minutes',
    'detail.hour1': '1 hour',
    'detail.day1': '1 day',
    'detail.shareHint':
      'Click Share on a row above to mint a short-lived presigned URL. No permanent public links are ever created.',
    'detail.copyUrl': 'Copy URL',
    'detail.dismiss': 'Dismiss',
    'detail.created': 'created {when}',
    'detail.makePrivate': 'Make private',
    'detail.makePublic': 'Make public-read',
    'detail.nowPrivate': 'Bucket is now private.',
    'detail.nowPublic': 'Bucket is now public-read.',
    'detail.pickKey': 'Pick an access key first (it needs read permission on this bucket).',
    'detail.urlReady': 'Signed URL ready — copy it now.',
    'detail.copyFailed': 'Copy failed — select the text manually.',
    'detail.bucketNotFound': 'Bucket not found',

    // --- relative time ---
    'time.justNow': 'just now',
    'time.minutesAgo': '{n}m ago',
    'time.hoursAgo': '{n}h ago',
    'time.daysAgo': '{n}d ago',
  },

  zh: {
    'common.loading': '加载中…',

    'app.tagline': '对象存储控制台',
    'app.signedInAs': '当前登录',
    'app.signOut': '退出登录',
    'lang.switch': '语言',

    'login.username': '用户名',
    'login.password': '密码',
    'login.submit': '登录',
    'login.submitting': '正在登录…',
    'login.failed': '登录失败',
    'login.insecure':
      '当前连接未被标记为安全，会话 Cookie 不会带上 Secure 属性。在网络环境下使用前，请把控制台放到 HTTPS 之后。',
    'login.region': '区域',

    'nav.overview': '概览',
    'nav.buckets': '存储桶',
    'nav.keys': '访问密钥',
    'nav.audit': '审计日志',
    'nav.settings': '设置',

    'overview.title': '概览',
    'overview.subtitle': '存储用量、凭据状态，以及最近的安全相关事件。',
    'overview.buckets': '存储桶',
    'overview.objects': '对象',
    'overview.stored': '已用空间',
    'overview.keys': '访问密钥',
    'overview.connectTitle': '客户端接入',
    'overview.connectLead':
      'hejiu45-oss 使用 path-style 寻址提供 S3 API。先创建访问密钥并授予某个存储桶的权限，然后用下面的 endpoint 接入。',
    'overview.snippetList': '# 列出存储桶',
    'overview.snippetTransfer': '# 上传 / 下载',
    'overview.snippetSdk': '# 其他 SDK',
    'overview.tlsNote':
      '当反向代理终止 TLS 时，endpoint 会使用带有效证书的 HTTPS（见 {file}）。',
    'overview.server': '服务状态',
    'overview.running': '运行中',
    'overview.dataDir': '数据目录',
    'overview.region': '区域',
    'overview.uptime': '运行时长',
    'overview.objectTypes': '对象类型',
    'overview.noObjects': '暂无对象',
    'overview.recent': '最近活动',
    'overview.viewAll': '查看全部',
    'overview.noActivity': '暂无活动记录。',
    'overview.colWhen': '时间',
    'overview.colActor': '操作者',
    'overview.colAction': '动作',
    'overview.colTarget': '对象',

    'buckets.title': '存储桶',
    'buckets.subtitle': '所有存储桶默认私有。访问密钥只能看到被授权给它的存储桶。',
    'buckets.new': '新建存储桶',
    'buckets.colBucket': '存储桶',
    'buckets.colVisibility': '可见性',
    'buckets.colObjects': '对象数',
    'buckets.colSize': '大小',
    'buckets.colKeys': '密钥数',
    'buckets.colCreated': '创建时间',
    'buckets.browse': '浏览',
    'buckets.delete': '删除',
    'buckets.empty': '还没有存储桶。',
    'buckets.createFirst': '创建第一个',
    'buckets.name': '名称',
    'buckets.nameHint':
      '3–63 个字符：小写字母、数字、点和连字符。不能看起来像 IP 地址。',
    'buckets.visibility': '可见性',
    'buckets.private': '私有 — 所有读取都需要签名请求（推荐）',
    'buckets.publicRead': '公开读 — 允许匿名 GET/HEAD',
    'buckets.cancel': '取消',
    'buckets.create': '创建存储桶',
    'buckets.creating': '正在创建…',
    'buckets.created': '存储桶 "{name}" 已创建。',
    'buckets.deleted': '存储桶 "{name}" 已删除。',
    'buckets.confirmDelete': '删除存储桶 "{name}"？此操作不可撤销。',
    'buckets.visibility.private': '私有',
    'buckets.visibility.public-read': '公开读',

    'keys.title': '访问密钥',
    'keys.subtitle':
      'S3 数据面使用的凭据。密钥以 AES-256-GCM 加密存储，且只在创建时显示一次。',
    'keys.issue': '签发新密钥',
    'keys.labelPlaceholder': '标签，例如 ci-deploy',
    'keys.create': '创建密钥',
    'keys.creating': '正在创建…',
    'keys.issueHint': '新密钥在下方授权之前没有任何存储桶权限 —— 默认最小权限。',
    'keys.colKey': '访问密钥',
    'keys.colLabel': '标签',
    'keys.colStatus': '状态',
    'keys.colBuckets': '已授权桶',
    'keys.colLastUsed': '最近使用',
    'keys.enabled': '已启用',
    'keys.disabled': '已禁用',
    'keys.none': '无',
    'keys.grants': '授权',
    'keys.disable': '禁用',
    'keys.enable': '启用',
    'keys.rotate': '轮换',
    'keys.delete': '删除',
    'keys.empty': '还没有访问密钥。',
    'keys.grantsFor': '{label} 的存储桶授权',
    'keys.read': '读',
    'keys.write': '写',
    'keys.createBucketFirst': '请先创建一个存储桶。',
    'keys.cancel': '取消',
    'keys.saveGrants': '保存授权',
    'keys.grantsSaved': '授权已保存。',
    'keys.storeSecret': '请立即保存此密钥',
    'keys.actionComplete': '操作完成',
    'keys.secretOnce': '密钥之后不会再显示，也无法找回 —— 只能轮换替换。',
    'keys.accessKeyId': 'Access Key ID',
    'keys.secretAccessKey': 'Secret Access Key',
    'keys.copyBoth': '复制两项',
    'keys.stored': '我已保存',
    'keys.rotated': '密钥已轮换，存储桶授权已迁移。',
    'keys.confirmRotate': '轮换 "{label}"？旧密钥会立即失效。',
    'keys.confirmDelete': '删除 "{label}" 及其所有授权？',
    'keys.copied': '已复制。',
    'keys.copyFailed': '复制失败。',

    'audit.title': '审计日志',
    'audit.subtitle': '按时间倒序，最多保留最近 200 条事件。存储在元数据数据库中。',
    'audit.colTime': '时间（UTC）',
    'audit.colActor': '操作者',
    'audit.colAction': '动作',
    'audit.colTarget': '对象',
    'audit.colDetail': '详情',
    'audit.colClient': '客户端',
    'audit.empty': '暂无事件记录。',

    'settings.title': '设置',
    'settings.subtitle': '控制台账号与服务器的安全状态。',
    'settings.posture': '安全状态',
    'settings.secureCookies': '安全 Cookie',
    'settings.enabled': '已启用',
    'settings.disabled': '未启用',
    'settings.secureCookiesHint':
      '需要进程内 TLS（--tls manual），或由受信任的反向代理（--tls reverse-proxy）报告 X-Forwarded-Proto: https。',
    'settings.bundle': '控制台资源',
    'settings.embedded': '已内嵌',
    'settings.placeholder': '占位页',
    'settings.bundleHint': '整个控制台都打包在二进制里，运行时不需要任何文件。',
    'settings.region': '区域',
    'settings.publicEndpoint': '对外地址',
    'settings.dataDir': '数据目录',
    'settings.changePassword': '修改密码',
    'settings.currentPassword': '当前密码',
    'settings.newPassword': '新密码（至少 12 个字符）',
    'settings.repeatPassword': '再次输入新密码',
    'settings.update': '修改密码',
    'settings.updating': '正在更新…',
    'settings.passwordHint': '修改密码会让所有设备退出登录，包括当前这台。',
    'settings.passwordMismatch': '两次输入的新密码不一致。',
    'settings.passwordTooShort': '请至少使用 12 个字符。',
    'settings.passwordChanged': '密码已修改。所有会话已失效 —— 请重新登录。',
    'settings.backups': '备份',
    'settings.backupsLead':
      '元数据和对象数据都在数据目录下。用内置命令做一次一致的离线备份：',
    'settings.backupsThen': '# 然后一并复制 objects/ 目录',
    'settings.backupsNote':
      '用 --restore-from 恢复到空的数据目录。解密已存储的 S3 密钥需要主密钥（master.key 或 HEJIU45_OSS_MASTER_KEY）—— 请单独备份它。',

    'detail.objects': '对象',
    'detail.filterPlaceholder': '前缀/过滤',
    'detail.filter': '过滤',
    'detail.parent': '上一级',
    'detail.colKey': '键',
    'detail.colType': '类型',
    'detail.colSize': '大小',
    'detail.colModified': '修改时间',
    'detail.prefix': '前缀',
    'detail.share': '分享',
    'detail.empty':
      '没有对象{under}。用 S3 API（aws s3 cp）上传 —— 上传走 SigV4 签名，绝不使用会话 Cookie。',
    'detail.emptyUnder': '（"{prefix}" 下）',
    'detail.access': '访问权限',
    'detail.accessLead': '读写权限是分开的。没有任何授权的密钥连列举这个桶都不允许。',
    'detail.colAccessKey': '访问密钥',
    'detail.colLabel': '标签',
    'detail.colRead': '读',
    'detail.colWrite': '写',
    'detail.yes': '是',
    'detail.no': '否',
    'detail.noAccess': '还没有密钥获得授权。请到',
    'detail.keysPage': '访问密钥',
    'detail.keysPageSuffix': '页面授权。',
    'detail.shareTitle': '分享对象',
    'detail.signingKey': '签名密钥（需要读权限）',
    'detail.choose': '— 请选择 —',
    'detail.lifetime': '有效期',
    'detail.min5': '5 分钟',
    'detail.min15': '15 分钟',
    'detail.hour1': '1 小时',
    'detail.day1': '1 天',
    'detail.shareHint':
      '在上方表格里点击「分享」，即可生成短时效的预签名 URL。系统不会创建永久公开链接。',
    'detail.copyUrl': '复制 URL',
    'detail.dismiss': '收起',
    'detail.created': '创建于 {when}',
    'detail.makePrivate': '改为私有',
    'detail.makePublic': '改为公开读',
    'detail.nowPrivate': '存储桶现在是私有。',
    'detail.nowPublic': '存储桶现在是公开读。',
    'detail.pickKey': '请先选择一个访问密钥（它需要这个桶的读权限）。',
    'detail.urlReady': '签名 URL 已生成 —— 请立即复制。',
    'detail.copyFailed': '复制失败 —— 请手动选中文本复制。',
    'detail.bucketNotFound': '找不到该存储桶',

    'time.justNow': '刚刚',
    'time.minutesAgo': '{n} 分钟前',
    'time.hoursAgo': '{n} 小时前',
    'time.daysAgo': '{n} 天前',
  },
}

export { messages }
