<script setup lang="ts">
import * as forge from 'node-forge'
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'

type Settings = {
  emby_host: string
  emby_api_key: string
  servers: EmbyServerConfig[]
  openlist_addr: string | null
  openlist_token: string | null
  port: number
  cache_ttl_seconds: number
  cache_max_capacity: number
  cache_domain_filter_mode: 'off' | 'whitelist' | 'blacklist'
  cache_domain_whitelist: string
  enable_internal_redirect: boolean
  internal_redirect_timeout_seconds: number
  strm_url_mappings: string
}

type RealIpMode = 'auto' | 'header' | 'header_list' | 'xff_last' | 'xff_second_last' | 'xff_third_last'

type EmbyServerConfig = {
  id: string
  name: string
  emby_host: string
  emby_api_key: string
  port: number
  enabled: boolean
  real_ip_mode: RealIpMode
  real_ip_header: string
}

type PublicKeyResponse = {
  algorithm: string
  public_key_pem: string
}

type AppInfo = {
  name: string
  version: string
  project_url: string
  ui_path: string
}

type Profile = {
  username: string
}

type PlaybackSession = {
  server_id: string
  server_name: string
  id: string
  user_name: string
  client: string
  device_name: string
  user_agent: string
  item_name: string
  series_name: string | null
  position_ticks: number | null
  runtime_ticks: number | null
  percent: number | null
  play_method: string | null
  transcoding: boolean
}

type MediaOverview = {
  movie_count: number
  series_count: number
  episode_count: number
  user_count: number
  server_name: string
  version: string
  operating_system: string
  library_count: number
}

type MediaOverviewTotals = {
  movie_count: number
  series_count: number
  episode_count: number
  user_count: number
  library_count: number
}

type ServerHealth = {
  uptime_seconds: number
  cpu_percent: number
  cpu_name: string
  cpu_cores: number
  memory_used_bytes: number
  memory_total_bytes: number
  memory_percent: number
  disk_used_bytes: number
  disk_total_bytes: number
  disk_percent: number
}

type DetailedHealth = {
  status: string
  name: string
  version: string
  database: string
  proxy_count: number
}

type ProxyStatus = {
  server_id: string
  server_name: string
  enabled: boolean
  port: number
  listening: boolean
  started_at_ms: number | null
  last_request_ms: number | null
  last_error: string | null
}

type RequestStatsDaily = {
  date: string
  server_id: string
  server_name: string
  port: number
  requests: number
  redirects: number
  cache_hits: number
  blocks: number
  errors: number
  updated_at_ms: number
}

type UpdateCheck = {
  current_version: string
  latest_version: string
  release_url: string
  has_update: boolean
  checked_at_ms: number
}

type ClientRuleRecord = {
  id: string
  client_name: string
  device_name: string
  user_name: string
  user_agent: string
  source: 'auto' | 'manual'
  enabled: boolean
  created_at: string
  updated_at: string
  note: string
}

type PlaybackRateBlockRecord = {
  id: string
  server_id: string
  server_name: string
  action: 'block_ip' | 'disable_user'
  ip: string
  user_name: string
  blocked_until: string
  created_at: string
  enabled: boolean
  note: string
}

type PlaybackRateWindowStatus = {
  server_id: string
  ip: string
  current_count: number
  threshold: number
  remaining: number
  window_seconds: number
  reset_at: string
  blocked: boolean
}

type WebhookNotifyConfig = {
  id: string
  enabled: boolean
  name: string
  url: string
  secret: string
}

type ClientControlConfig = {
  enabled: boolean
  notify_enabled: boolean
  playback_rate_limit_enabled: boolean
  playback_rate_limit_window_seconds: number
  playback_rate_limit_max_requests: number
  playback_rate_limit_block_seconds: number
  playback_rate_limit_action: 'block_ip' | 'disable_user'
  rate_limit_blocks: PlaybackRateBlockRecord[]
  webhook?: WebhookNotifyConfig
  webhooks: WebhookNotifyConfig[]
  records: ClientRuleRecord[]
}

type ActivityLogEntry = {
  id: number
  timestamp_ms: number
  kind: 'playback' | 'general'
  level: 'success' | 'info' | 'warn' | 'error'
  server_id: string | null
  server_name: string
  playback_user: string | null
  playback_ip: string | null
  message: string
  detail: string
}

type AuditLogEntry = {
  id: number
  timestamp_ms: number
  admin_user_id: number | null
  admin_username: string
  action: string
  summary: string
  result: string
}

type ValidationResult = {
  scope: string
  ok: boolean
  message: string
  detail: string
}

type ValidationResponse = {
  ok: boolean
  results: ValidationResult[]
}

type SystemLogConfig = {
  debug_mode: boolean
  level: 'debug' | 'info' | 'warning' | 'error' | 'critical'
  max_size_mb: number
  max_backups: number
  format: string
}

type AuthMode = 'loading' | 'setup' | 'login' | 'app'
type Page = 'home' | 'server' | 'clients' | 'notifications' | 'backup' | 'logs' | 'account'
type ClientStatusFilter = 'all' | 'blocked' | 'allowed'
type LogKindFilter = 'all' | 'playback' | 'general'
type EncryptionPublicKey =
  | { kind: 'webcrypto'; key: CryptoKey }
  | { kind: 'forge'; key: forge.pki.rsa.PublicKey }

const tokenKey = 'embypanel_token'
const pageKey = 'embypanel_page'
const themeKey = 'embypanel_theme'
const validPages: Page[] = ['home', 'server', 'clients', 'logs', 'notifications', 'backup', 'account']
const mode = ref<AuthMode>('loading')
const page = ref<Page>(storedPage())
const token = ref(storedToken())
const darkMode = ref(storedTheme() === 'dark')
const saving = ref(false)
const restartingServerId = ref('')
const changingPassword = ref(false)
const savingProfile = ref(false)
const error = ref('')
const notice = ref('')
const publicKey = ref<EncryptionPublicKey | null>(null)
const playbackSessions = ref<PlaybackSession[]>([])
const playbackLoading = ref(false)
const playbackError = ref('')
const activityLogs = ref<ActivityLogEntry[]>([])
const logsLoading = ref(false)
const logsError = ref('')
const selectedLogServer = ref('all')
const selectedLogKind = ref<LogKindFilter>('all')
const selectedLogLevel = ref('all')
const logKeywordFilter = ref('')
const logSince = ref('')
const logUntil = ref('')
const mediaOverviews = ref<MediaOverview[]>([])
const serverHealth = ref<ServerHealth | null>(null)
const detailedHealth = ref<DetailedHealth | null>(null)
const proxyStatuses = ref<ProxyStatus[]>([])
const requestStats = ref<RequestStatsDaily[]>([])
const updateCheck = ref<UpdateCheck | null>(null)
const validationResults = ref<ValidationResult[]>([])
const rateLimitWindows = ref<PlaybackRateWindowStatus[]>([])
const auditLogs = ref<AuditLogEntry[]>([])
const auditKeywordFilter = ref('')
const selectedAuditAction = ref('all')
const overviewError = ref('')
const healthError = ref('')
const clientControl = reactive<ClientControlConfig>({
  enabled: false,
  notify_enabled: false,
  playback_rate_limit_enabled: false,
  playback_rate_limit_window_seconds: 60,
  playback_rate_limit_max_requests: 20,
  playback_rate_limit_block_seconds: 1800,
  playback_rate_limit_action: 'block_ip',
  rate_limit_blocks: [],
  webhooks: [{
    id: newWebhookId(),
    enabled: false,
    name: '新建 Webhook',
    url: '',
    secret: '',
  }],
  records: [],
})
const savingClientControl = ref(false)
const testingWebhook = ref(false)
const addingClientRule = ref(false)
const clientControlError = ref('')
const clientStatusFilter = ref<ClientStatusFilter>('all')
const clientKeywordFilter = ref('')
const visibleApiKeyServers = ref<Record<string, boolean>>({})
const backupError = ref('')
const backupFileInput = ref<HTMLInputElement | null>(null)
const logConfig = reactive<SystemLogConfig>({
  debug_mode: false,
  level: 'info',
  max_size_mb: 5,
  max_backups: 10,
  format: '[%(levelname)s] %(asctime)s - %(message)s',
})
let dashboardTimer: number | undefined
let logsTimer: number | undefined

const credentials = reactive({
  username: '',
  password: '',
})

const profile = reactive<Profile>({
  username: '',
})

const profileForm = reactive({
  username: '',
})

const passwordForm = reactive({
  current_password: '',
  new_password: '',
  confirm_password: '',
})

const appInfo = reactive<AppInfo>({
  name: 'EmbyPanel',
  version: '',
  project_url: '',
  ui_path: '/ui/',
})

const manualClientRule = reactive({
  user_agent: '',
  note: '',
})

const settings = reactive<Settings>({
  emby_host: '',
  emby_api_key: '',
  servers: [],
  openlist_addr: null,
  openlist_token: null,
  port: 8096,
  cache_ttl_seconds: 180,
  cache_max_capacity: 10000,
  cache_domain_filter_mode: 'off',
  cache_domain_whitelist: '',
  enable_internal_redirect: false,
  internal_redirect_timeout_seconds: 15,
  strm_url_mappings: '',
})

const menu = [
  { id: 'home' as const, label: '首页', icon: '⌂' },
  { id: 'server' as const, label: '服务器', icon: '▣' },
  { id: 'clients' as const, label: '客户端', icon: '◫' },
  { id: 'logs' as const, label: '日志', icon: '≡' },
  { id: 'notifications' as const, label: '通知', icon: '◇' },
  { id: 'backup' as const, label: '备份', icon: '▤' },
  { id: 'account' as const, label: '账户', icon: '◎' },
]

const realIpModeOptions: Array<{ value: RealIpMode; label: string }> = [
  { value: 'auto', label: '系统识别' },
  { value: 'header', label: '从 HTTP Header 中获取' },
  { value: 'header_list', label: '从 Header 列表中获取' },
  { value: 'xff_last', label: '获取 X-Forwarded-For 的上一级代理地址' },
  { value: 'xff_second_last', label: '获取 X-Forwarded-For 的上上一级代理地址' },
  { value: 'xff_third_last', label: '获取 X-Forwarded-For 的上上上一级代理地址' },
]

const playbackLimitActionOptions: Array<{ value: ClientControlConfig['playback_rate_limit_action']; label: string; description: string }> = [
  { value: 'block_ip', label: '屏蔽 IP', description: '屏蔽频繁播放的 IP' },
  { value: 'disable_user', label: '禁用用户', description: '通过 API 禁用该用户' },
]

const defaultCdnHeaders = [
  'x-forwarded-for',
  'x-real-ip',
  'x-forwarded',
  'forwarded-for',
  'forwarded',
  'true-client-ip',
  'client-ip',
  'ali-cdn-real-ip',
  'cdn-src-ip',
  'cdn-real-ip',
  'cf-connecting-ip',
  'x-cluster-client-ip',
  'wl-proxy-client-ip',
  'proxy-client-ip',
].join('\n')

const activePlayCount = computed(() => playbackSessions.value.length)
const logServers = computed(() =>
  settings.servers.map((server) => ({
    id: server.id,
    name: server.name || `端口 ${server.port}`,
    port: server.port,
    enabled: server.enabled,
  })),
)
const filteredActivityLogs = computed(() =>
  activityLogs.value.filter((entry) => selectedLogKind.value === 'all' || entry.kind === selectedLogKind.value),
)
const playbackLogRows = computed(() => filteredActivityLogs.value.filter((entry) => entry.kind === 'playback'))
const generalLogRows = computed(() =>
  filteredActivityLogs.value.filter((entry) => entry.kind === 'general' && entry.level === 'info'),
)
const clientRuleRows = computed(() =>
  [...clientControl.records]
    .filter((record) => {
      if (clientStatusFilter.value === 'blocked') return record.enabled
      if (clientStatusFilter.value === 'allowed') return !record.enabled
      return true
    })
    .filter((record) => {
      const keyword = clientKeywordFilter.value.trim().toLowerCase()
      if (!keyword) return true
      return [
        record.user_agent,
        record.client_name,
        record.device_name,
        record.user_name,
        record.note,
      ].some((value) => value.toLowerCase().includes(keyword))
    })
    .sort((left, right) => Number(right.updated_at) - Number(left.updated_at)),
)
const blockedClientCount = computed(() => clientControl.records.filter((record) => record.enabled).length)
const allowedClientCount = computed(() => clientControl.records.length - blockedClientCount.value)
const activeRateLimitBlocks = computed(() =>
  [...clientControl.rate_limit_blocks]
    .filter((record) => record.enabled)
    .sort((left, right) => Number(right.created_at) - Number(left.created_at)),
)
const requestStatsTotals = computed(() =>
  requestStats.value.reduce(
    (totals, row) => ({
      requests: totals.requests + row.requests,
      redirects: totals.redirects + row.redirects,
      cache_hits: totals.cache_hits + row.cache_hits,
      blocks: totals.blocks + row.blocks,
      errors: totals.errors + row.errors,
    }),
    { requests: 0, redirects: 0, cache_hits: 0, blocks: 0, errors: 0 },
  ),
)
const rateLimitOverview = computed(() => ({
  active_windows: rateLimitWindows.value.length,
  blocked_windows: rateLimitWindows.value.filter((row) => row.blocked).length,
  highest_count: rateLimitWindows.value.reduce((max, row) => Math.max(max, row.current_count), 0),
}))
const proxyStatusById = computed(() =>
  Object.fromEntries(proxyStatuses.value.map((status) => [status.server_id, status])),
)
const auditActionOptions = computed(() => {
  const actions = new Set(auditLogs.value.map((entry) => entry.action))
  return ['all', ...actions]
})
const mediaOverviewTotals = computed<MediaOverviewTotals>(() =>
  mediaOverviews.value.reduce(
    (totals, overview) => ({
      movie_count: totals.movie_count + overview.movie_count,
      series_count: totals.series_count + overview.series_count,
      episode_count: totals.episode_count + overview.episode_count,
      user_count: totals.user_count + overview.user_count,
      library_count: totals.library_count + overview.library_count,
    }),
    { movie_count: 0, series_count: 0, episode_count: 0, user_count: 0, library_count: 0 },
  ),
)

function storedPage(): Page {
  const stored = readStorage(localStorage, pageKey)
  return validPages.includes(stored as Page) ? (stored as Page) : 'home'
}

function setPage(nextPage: Page) {
  page.value = nextPage
  writeStorage(localStorage, pageKey, nextPage)
  if (nextPage === 'logs') void refreshActivityLogs()
}

function storedTheme() {
  return readStorage(localStorage, themeKey) || 'light'
}

function toggleTheme() {
  darkMode.value = !darkMode.value
  writeStorage(localStorage, themeKey, darkMode.value ? 'dark' : 'light')
}

async function bootstrap() {
  mode.value = 'loading'
  error.value = ''
  try {
    await refreshAppInfo()
    publicKey.value = await fetchPublicKey()
    const status = await api<{ initialized: boolean }>('/api/setup-status')
    if (!status.initialized) {
      mode.value = 'setup'
      return
    }
    if (!token.value) {
      mode.value = 'login'
      return
    }
    storeToken(token.value)
    await loadAppData()
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
    mode.value = token.value ? 'app' : 'login'
  }
}

async function refreshAppInfo() {
  try {
    Object.assign(appInfo, await api<AppInfo>('/api/app-info'))
  } catch {
    appInfo.version = ''
  }
}

async function setupAdmin() {
  await authenticate('/api/setup')
}

async function login() {
  await authenticate('/api/login')
}

async function authenticate(path: string) {
  saving.value = true
  error.value = ''
  try {
    const response = await api<{ token: string }>(path, {
      method: 'POST',
      body: JSON.stringify(await encryptPayload('credentials', { ...credentials })),
    })
    token.value = response.token
    storeToken(token.value)
    await loadAppData()
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
  } finally {
    saving.value = false
  }
}

async function loadAppData() {
  error.value = ''
  const [settingsResponse, profileResponse, clientControlResponse] = await Promise.all([
    api<Settings>('/api/settings'),
    api<Profile>('/api/profile'),
    api<ClientControlConfig>('/api/client-control'),
  ])
  Object.assign(settings, settingsResponse)
  normalizeSettingsServers()
  Object.assign(profile, profileResponse)
  applyClientControlConfig(clientControlResponse)
  profileForm.username = profile.username
  mode.value = 'app'
  void refreshUpdateCheck()
  await refreshOperationalData()
  await refreshLogConfig()
  await refreshRateLimitStatus()
  await refreshAuditLogs()
  await refreshDashboard()
  await refreshActivityLogs()
  startDashboardPolling()
}

async function refreshOperationalData() {
  try {
    const [health, statuses, stats, rateLimit] = await Promise.all([
      api<DetailedHealth>('/api/monitoring/healthz'),
      api<ProxyStatus[]>('/api/monitoring/proxy-status'),
      api<RequestStatsDaily[]>('/api/monitoring/stats'),
      api<PlaybackRateWindowStatus[]>('/api/client-control/rate-limit/status'),
    ])
    detailedHealth.value = health
    proxyStatuses.value = statuses
    requestStats.value = stats
    rateLimitWindows.value = rateLimit
  } catch (err) {
    healthError.value = err instanceof Error ? err.message : String(err)
  }
}

async function refreshUpdateCheck() {
  try {
    updateCheck.value = await api<UpdateCheck>('/api/app-info/update-check')
  } catch {
    updateCheck.value = null
  }
}

async function refreshLogConfig() {
  try {
    Object.assign(logConfig, await api<SystemLogConfig>('/api/settings/log-config'))
  } catch {
    // Log config remains on defaults when the backend is not ready.
  }
}

async function refreshClientControl() {
  clientControlError.value = ''
  try {
    const response = await api<ClientControlConfig>('/api/client-control')
    applyClientControlConfig(response)
  } catch (err) {
    clientControlError.value = err instanceof Error ? err.message : String(err)
  }
}

async function refreshRateLimitStatus() {
  try {
    rateLimitWindows.value = await api<PlaybackRateWindowStatus[]>('/api/client-control/rate-limit/status')
  } catch {
    rateLimitWindows.value = []
  }
}

async function refreshAuditLogs() {
  try {
    const params = new URLSearchParams({ limit: '120' })
    if (selectedAuditAction.value !== 'all') params.set('action', selectedAuditAction.value)
    if (auditKeywordFilter.value.trim()) params.set('keyword', auditKeywordFilter.value.trim())
    auditLogs.value = await api<AuditLogEntry[]>(`/api/monitoring/audit-logs?${params.toString()}`)
  } catch {
    auditLogs.value = []
  }
}

async function saveSettings() {
  saving.value = true
  notice.value = ''
  error.value = ''
  try {
    const payload = buildSettingsPayload()
    const response = await api<Settings>('/api/settings', {
      method: 'PUT',
      body: JSON.stringify(await encryptPayload('settings', payload)),
    })
    Object.assign(settings, response)
    normalizeSettingsServers()
    notice.value = '服务器配置已保存，反代服务已重启'
    await refreshOperationalData()
    await refreshDashboard()
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
  } finally {
    saving.value = false
  }
}

async function validateSettings() {
  saving.value = true
  error.value = ''
  notice.value = ''
  validationResults.value = []
  try {
    const payload = buildSettingsPayload()
    const response = await api<ValidationResponse>('/api/settings/validate', {
      method: 'POST',
      body: JSON.stringify(await encryptPayload('settings', payload)),
    })
    validationResults.value = response.results
    notice.value = response.ok ? '配置测试通过' : '配置测试完成，请查看警告项'
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
  } finally {
    saving.value = false
  }
}

async function exportBackup() {
  backupError.value = ''
  notice.value = ''
  try {
    const response = await api<{ backup: string }>('/api/settings/backup/export', {
      method: 'POST',
    })
    downloadTextFile(response.backup, backupFileName())
    notice.value = '配置文件已生成，请在浏览器下载记录中查看'
  } catch (err) {
    backupError.value = err instanceof Error ? err.message : String(err)
  }
}

async function importBackup() {
  backupError.value = ''
  notice.value = ''
  backupFileInput.value?.click()
}

async function handleBackupFileSelected(event: Event) {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = ''
  if (!file) return
  backupError.value = ''
  notice.value = ''
  try {
    const backup = await file.text()
    await importBackupText(backup)
  } catch (err) {
    backupError.value = err instanceof Error ? err.message : String(err)
  }
}

async function importBackupText(backupText: string) {
  backupError.value = ''
  notice.value = ''
  const backup = backupText.trim()
  if (!backup) {
    backupError.value = '配置文件内容为空'
    return
  }
  const confirmed = window.confirm('还原配置文件会覆盖当前配置并重启反代服务，确定继续吗？')
  if (!confirmed) return
  try {
    const response = await api<Settings>('/api/settings/backup/import', {
      method: 'POST',
      body: JSON.stringify(await encryptPayload('backup', { backup })),
    })
    Object.assign(settings, response)
    normalizeSettingsServers()
    await refreshOperationalData()
    notice.value = '配置文件已还原，反代服务已重启'
  } catch (err) {
    backupError.value = err instanceof Error ? err.message : String(err)
  }
}

function downloadTextFile(content: string, filename: string) {
  const blob = new Blob([content], { type: 'application/json;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = filename
  document.body.appendChild(link)
  link.click()
  link.remove()
  URL.revokeObjectURL(url)
}

function backupFileName() {
  const timestamp = new Date()
    .toISOString()
    .replace(/\.\d{3}Z$/, '')
    .replace(/[-:T]/g, '')
  return `embypanel-config-${timestamp}.json`
}

async function saveLogConfig() {
  logsError.value = ''
  notice.value = ''
  try {
    Object.assign(logConfig, await api<SystemLogConfig>('/api/settings/log-config', {
      method: 'PUT',
      body: JSON.stringify(await encryptPayload('log_config', { ...logConfig })),
    }))
    notice.value = '日志配置已保存'
  } catch (err) {
    logsError.value = err instanceof Error ? err.message : String(err)
  }
}

async function exportLogs() {
  const params = logQueryParams(500)
  const headers = new Headers()
  if (token.value) headers.set('Authorization', `Bearer ${token.value}`)
  const response = await fetch(`/api/monitoring/logs/export?${params.toString()}`, { headers })
  if (!response.ok) {
    logsError.value = await response.text()
    return
  }
  const blob = await response.blob()
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = 'embypanel-logs.csv'
  document.body.appendChild(link)
  link.click()
  link.remove()
  URL.revokeObjectURL(url)
}

async function restartProxyServer(server: EmbyServerConfig) {
  restartingServerId.value = server.id
  notice.value = ''
  error.value = ''
  try {
    const response = await api<Settings>('/api/settings/restart-proxy', {
      method: 'POST',
      body: JSON.stringify({ server_id: server.id }),
    })
    Object.assign(settings, response)
    normalizeSettingsServers()
    notice.value = `${server.name || '服务器'} 反代服务已重启`
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
  } finally {
    restartingServerId.value = ''
  }
}

function buildSettingsPayload() {
  const servers = settings.servers.map((server) => ({
    ...server,
    name: server.name.trim(),
    emby_host: server.emby_host.trim(),
    emby_api_key: server.emby_api_key.trim(),
    port: Number(server.port),
    real_ip_mode: server.real_ip_mode || 'auto',
    real_ip_header: server.real_ip_header.trim(),
  }))
  const primary = servers.find((server) => server.enabled) ?? servers[0]
  return {
      ...settings,
      servers,
      emby_host: primary?.emby_host ?? settings.emby_host,
      emby_api_key: primary?.emby_api_key ?? settings.emby_api_key,
      port: primary?.port ?? settings.port,
      openlist_addr: emptyToNull(settings.openlist_addr),
      openlist_token: emptyToNull(settings.openlist_token),
    }
}

function normalizeSettingsServers() {
  if (!settings.servers.length) {
    settings.servers = [
      {
        id: newServerId(),
        name: '默认服务器',
        emby_host: settings.emby_host,
        emby_api_key: settings.emby_api_key,
        port: settings.port || 8096,
        enabled: true,
        real_ip_mode: 'auto',
        real_ip_header: '',
      },
    ]
    return
  }
  settings.servers = settings.servers.map((server) => ({
    ...server,
    real_ip_mode: server.real_ip_mode || 'auto',
    real_ip_header: server.real_ip_header || '',
  }))
}

function addServer() {
  const lastPort = settings.servers.at(-1)?.port ?? 8096
  settings.servers.push({
    id: newServerId(),
    name: `服务器 ${settings.servers.length + 1}`,
    emby_host: '',
    emby_api_key: '',
    port: lastPort + 1,
    enabled: true,
    real_ip_mode: 'auto',
    real_ip_header: '',
  })
}

function needsRealIpHeader(server: EmbyServerConfig) {
  return server.real_ip_mode === 'header' || server.real_ip_mode === 'header_list'
}

function updateRealIpMode(server: EmbyServerConfig) {
  if (server.real_ip_mode === 'header_list' && !server.real_ip_header.trim()) {
    server.real_ip_header = defaultCdnHeaders
  }
}

function removeServer(serverId: string) {
  if (settings.servers.length <= 1) {
    error.value = '至少保留一个服务器配置'
    return
  }
  const confirmed = window.confirm('确定删除这个服务器配置吗？对应反代端口保存后会停止监听。')
  if (!confirmed) return
  settings.servers = settings.servers.filter((server) => server.id !== serverId)
  const { [serverId]: _removed, ...visibleServers } = visibleApiKeyServers.value
  visibleApiKeyServers.value = visibleServers
}

function newServerId() {
  const bytes = randomBytes(8)
  return `server-${bytesToBase64Url(bytes)}`
}

function isApiKeyVisible(serverId: string) {
  return Boolean(visibleApiKeyServers.value[serverId])
}

function toggleApiKeyVisible(serverId: string) {
  visibleApiKeyServers.value = {
    ...visibleApiKeyServers.value,
    [serverId]: !visibleApiKeyServers.value[serverId],
  }
}

async function saveProfile() {
  savingProfile.value = true
  notice.value = ''
  error.value = ''
  try {
    const response = await api<Profile>('/api/profile', {
      method: 'PUT',
      body: JSON.stringify(await encryptPayload('profile', { username: profileForm.username })),
    })
    Object.assign(profile, response)
    profileForm.username = response.username
    notice.value = '账户资料已更新'
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
  } finally {
    savingProfile.value = false
  }
}

async function saveClientControl() {
  savingClientControl.value = true
  notice.value = ''
  clientControlError.value = ''
  try {
    const response = await api<ClientControlConfig>('/api/client-control', {
      method: 'PUT',
      body: JSON.stringify(await encryptPayload('client_control', sanitizeClientControl())),
    })
    applyClientControlConfig(response)
    notice.value = '客户端管控规则已保存'
  } catch (err) {
    clientControlError.value = err instanceof Error ? err.message : String(err)
  } finally {
    savingClientControl.value = false
  }
}

function sanitizeClientControl() {
  return {
    enabled: clientControl.enabled,
    notify_enabled: clientControl.notify_enabled,
    playback_rate_limit_enabled: clientControl.playback_rate_limit_enabled,
    playback_rate_limit_window_seconds: Number(clientControl.playback_rate_limit_window_seconds),
    playback_rate_limit_max_requests: Number(clientControl.playback_rate_limit_max_requests),
    playback_rate_limit_block_seconds: Number(clientControl.playback_rate_limit_block_seconds),
    playback_rate_limit_action: clientControl.playback_rate_limit_action || 'block_ip',
    webhooks: clientControl.webhooks.map((webhook) => ({
      id: webhook.id || newWebhookId(),
      enabled: webhook.enabled,
      name: webhook.name.trim(),
      url: webhook.url.trim(),
      secret: webhook.secret.trim(),
    })),
  }
}

async function testWebhook(webhook: WebhookNotifyConfig) {
  clientControlError.value = ''
  notice.value = ''
  const url = webhook.url.trim()
  if (!url) {
    clientControlError.value = 'Webhook URL 不能为空'
    return
  }
  testingWebhook.value = true
  try {
    await api<{ ok: boolean }>('/api/client-control/webhook/test', {
      method: 'POST',
      body: JSON.stringify(
        await encryptPayload('webhook_test', {
          url,
          secret: webhook.secret.trim() || null,
          title: 'EmbyPanel 通知测试',
          text: 'Webhook POST 测试成功',
        }),
      ),
    })
    notice.value = 'Webhook 测试发送成功'
  } catch (err) {
    clientControlError.value = err instanceof Error ? err.message : String(err)
  } finally {
    testingWebhook.value = false
  }
}

function applyClientControlConfig(response: ClientControlConfig) {
  Object.assign(clientControl, response)
  const webhooks = response.webhooks?.length
    ? response.webhooks
    : response.webhook
      ? [response.webhook]
      : []
  clientControl.webhooks = webhooks.length
    ? webhooks.map(normalizeWebhook)
    : [newWebhookConfig()]
}

function newWebhookConfig(): WebhookNotifyConfig {
  return {
    id: newWebhookId(),
    enabled: true,
    name: '新建 Webhook',
    url: '',
    secret: '',
  }
}

function normalizeWebhook(webhook: Partial<WebhookNotifyConfig>): WebhookNotifyConfig {
  return {
    id: webhook.id?.trim() || newWebhookId(),
    enabled: Boolean(webhook.enabled),
    name: webhook.name?.trim() || '新建 Webhook',
    url: webhook.url?.trim() || '',
    secret: webhook.secret?.trim() || '',
  }
}

function addWebhook() {
  clientControl.webhooks.push(newWebhookConfig())
}

function removeWebhook(index: number) {
  if (clientControl.webhooks.length <= 1) {
    clientControl.webhooks = [newWebhookConfig()]
    return
  }
  clientControl.webhooks.splice(index, 1)
}

function newWebhookId() {
  const bytes = randomBytes(8)
  return `webhook-${bytesToBase64Url(bytes)}`
}

async function addClientRule() {
  clientControlError.value = ''
  notice.value = ''
  const userAgent = manualClientRule.user_agent.trim()
  if (!userAgent) {
    clientControlError.value = 'UA 关键字不能为空'
    return
  }
  addingClientRule.value = true
  try {
    const response = await api<ClientControlConfig>('/api/client-control/rules', {
      method: 'POST',
      body: JSON.stringify(
        await encryptPayload('client_rule', {
          user_agent: userAgent,
          note: manualClientRule.note.trim() || null,
        }),
      ),
    })
    applyClientControlConfig(response)
    manualClientRule.user_agent = ''
    manualClientRule.note = ''
    notice.value = 'UA 拦截规则已添加'
  } catch (err) {
    clientControlError.value = err instanceof Error ? err.message : String(err)
  } finally {
    addingClientRule.value = false
  }
}

async function toggleClientRule(record: ClientRuleRecord) {
  clientControlError.value = ''
  try {
    const response = await api<ClientControlConfig>('/api/client-control/rules/toggle', {
      method: 'PUT',
      body: JSON.stringify(
        await encryptPayload('client_rule', {
          id: record.id,
          enabled: !record.enabled,
        }),
      ),
    })
    applyClientControlConfig(response)
  } catch (err) {
    clientControlError.value = err instanceof Error ? err.message : String(err)
  }
}

async function unblockRateLimit(record: PlaybackRateBlockRecord) {
  clientControlError.value = ''
  try {
    const response = await api<ClientControlConfig>('/api/client-control/rate-blocks/unblock', {
      method: 'POST',
      body: JSON.stringify(await encryptPayload('rate_limit_block', { id: record.id })),
    })
    applyClientControlConfig(response)
    notice.value = record.action === 'disable_user' ? '用户封禁已解除' : 'IP 屏蔽已解除'
  } catch (err) {
    clientControlError.value = err instanceof Error ? err.message : String(err)
  }
}

async function changePassword() {
  error.value = ''
  notice.value = ''
  if (!passwordForm.new_password) {
    error.value = '新密码不能为空'
    return
  }
  if (passwordForm.new_password !== passwordForm.confirm_password) {
    error.value = '两次输入的新密码不一致'
    return
  }
  changingPassword.value = true
  try {
    await api<{ changed: boolean }>('/api/change-password', {
      method: 'POST',
      body: JSON.stringify(
        await encryptPayload('password', {
          current_password: passwordForm.current_password,
          new_password: passwordForm.new_password,
        }),
      ),
    })
    passwordForm.current_password = ''
    passwordForm.new_password = ''
    passwordForm.confirm_password = ''
    notice.value = '管理员密码已更新'
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
  } finally {
    changingPassword.value = false
  }
}

async function deleteClientRule(record: ClientRuleRecord) {
  clientControlError.value = ''
  const confirmed = window.confirm(`确定删除 UA 规则「${clientKeyword(record)}」吗？`)
  if (!confirmed) return
  try {
    const response = await api<ClientControlConfig>('/api/client-control/rules', {
      method: 'DELETE',
      body: JSON.stringify(
        await encryptPayload('client_rule', {
          id: record.id,
        }),
      ),
    })
    applyClientControlConfig(response)
    notice.value = 'UA 规则已删除'
  } catch (err) {
    clientControlError.value = err instanceof Error ? err.message : String(err)
  }
}

function clearClientFilters() {
  clientStatusFilter.value = 'all'
  clientKeywordFilter.value = ''
}

function rateLimitBlockIp(record: PlaybackRateBlockRecord) {
  const ip = record.ip?.trim()
  if (ip) return ip
  return record.note.match(/(?:IP|ip)\s+([0-9a-fA-F:.]+)/)?.[1] ?? '--'
}

function logout() {
  token.value = ''
  page.value = 'home'
  clearStoredToken()
  removeStorage(localStorage, pageKey)
  stopDashboardPolling()
  mode.value = 'login'
}

function storedToken() {
  return readStorage(localStorage, tokenKey) || readStorage(sessionStorage, tokenKey)
}

function storeToken(value: string) {
  writeStorage(localStorage, tokenKey, value)
  writeStorage(sessionStorage, tokenKey, value)
}

function clearStoredToken() {
  removeStorage(localStorage, tokenKey)
  removeStorage(sessionStorage, tokenKey)
}

function readStorage(storage: Storage, key: string) {
  try {
    return storage.getItem(key) ?? ''
  } catch {
    return ''
  }
}

function writeStorage(storage: Storage, key: string, value: string) {
  try {
    storage.setItem(key, value)
  } catch {
    // Ignore restricted storage; the in-memory token remains valid for this tab.
  }
}

function removeStorage(storage: Storage, key: string) {
  try {
    storage.removeItem(key)
  } catch {
    // Ignore restricted storage during logout.
  }
}

async function refreshDashboard() {
  await Promise.all([refreshOverview(), refreshHealth(), refreshPlaybackSessions(), refreshOperationalData()])
}

async function refreshOverview() {
  if (!token.value) return
  overviewError.value = ''
  try {
    mediaOverviews.value = await api<MediaOverview[]>('/api/monitoring/overview')
  } catch (err) {
    mediaOverviews.value = []
    overviewError.value = err instanceof Error ? err.message : String(err)
  }
}

async function refreshHealth() {
  if (!token.value) return
  healthError.value = ''
  try {
    serverHealth.value = await api<ServerHealth>('/api/monitoring/health')
  } catch (err) {
    serverHealth.value = null
    healthError.value = err instanceof Error ? err.message : String(err)
  }
}

async function refreshPlaybackSessions() {
  if (!token.value) return
  playbackLoading.value = true
  playbackError.value = ''
  try {
    playbackSessions.value = await api<PlaybackSession[]>('/api/monitoring/plays')
    if (page.value === 'clients') await refreshClientControl()
  } catch (err) {
    playbackSessions.value = []
    playbackError.value = err instanceof Error ? err.message : String(err)
  } finally {
    playbackLoading.value = false
  }
}

async function refreshActivityLogs() {
  if (!token.value) return
  logsLoading.value = true
  logsError.value = ''
  try {
    if (selectedLogKind.value === 'all') {
      const [playback, info] = await Promise.all([
        fetchActivityLogs('playback', 120),
        fetchActivityLogs('general', 80),
      ])
      activityLogs.value = [...playback, ...info].sort((left, right) => right.timestamp_ms - left.timestamp_ms)
    } else {
      activityLogs.value = await fetchActivityLogs(selectedLogKind.value, 160)
    }
  } catch (err) {
    activityLogs.value = []
    logsError.value = err instanceof Error ? err.message : String(err)
  } finally {
    logsLoading.value = false
  }
}

async function fetchActivityLogs(kind: LogKindFilter, limit: number) {
  const params = logQueryParams(limit)
  if (kind !== 'all') params.set('kind', kind)
  return api<ActivityLogEntry[]>(`/api/monitoring/logs?${params.toString()}`)
}

function logQueryParams(limit: number) {
  const params = new URLSearchParams({ limit: String(limit) })
  if (selectedLogServer.value !== 'all') params.set('server_id', selectedLogServer.value)
  if (selectedLogKind.value !== 'all') params.set('kind', selectedLogKind.value)
  if (selectedLogLevel.value !== 'all') params.set('level', selectedLogLevel.value)
  if (logKeywordFilter.value.trim()) params.set('keyword', logKeywordFilter.value.trim())
  if (logSince.value) params.set('since_ms', String(new Date(logSince.value).getTime()))
  if (logUntil.value) params.set('until_ms', String(new Date(logUntil.value).getTime()))
  return params
}

function startDashboardPolling() {
  stopDashboardPolling()
  dashboardTimer = window.setInterval(refreshDashboard, 10000)
  logsTimer = window.setInterval(() => {
    if (page.value === 'logs') {
      void (async () => {
        await refreshPlaybackSessions()
        await refreshActivityLogs()
      })()
    }
  }, 3000)
}

function stopDashboardPolling() {
  if (dashboardTimer !== undefined) {
    window.clearInterval(dashboardTimer)
    dashboardTimer = undefined
  }
  if (logsTimer !== undefined) {
    window.clearInterval(logsTimer)
    logsTimer = undefined
  }
}

async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers)
  headers.set('Content-Type', 'application/json')
  if (token.value) headers.set('Authorization', `Bearer ${token.value}`)
  const response = await fetch(path, { ...init, headers })
  if (!response.ok) {
    const message = await response.text()
    if (response.status === 401 && token.value && !isAuthBootstrapPath(path)) {
      handleAuthExpired()
    }
    throw new Error(message)
  }
  return response.json() as Promise<T>
}

function isAuthBootstrapPath(path: string) {
  return path === '/api/login' || path === '/api/setup' || path === '/api/setup-status'
}

function handleAuthExpired() {
  token.value = ''
  clearStoredToken()
  stopDashboardPolling()
  mode.value = 'login'
  error.value = '登录已过期，请重新登录'
}

async function fetchPublicKey() {
  const response = await api<PublicKeyResponse>('/api/public-key')
  if (!hasWebCrypto()) {
    return {
      kind: 'forge' as const,
      key: forge.pki.publicKeyFromPem(response.public_key_pem),
    }
  }
  return {
    kind: 'webcrypto' as const,
    key: await crypto.subtle.importKey(
      'spki',
      pemToArrayBuffer(response.public_key_pem),
      { name: 'RSA-OAEP', hash: 'SHA-256' },
      false,
      ['encrypt'],
    ),
  }
}

async function encryptPayload(name: string, value: unknown) {
  publicKey.value = await fetchPublicKey()
  const aesKey = randomBytes(32)
  const fieldName = randomFieldName()
  const iv = randomBytes(12)
  const plaintext = new TextEncoder().encode(JSON.stringify({ name, value }))
  const { encryptedKey, encrypted } =
    publicKey.value.kind === 'webcrypto'
      ? await encryptWithWebCrypto(publicKey.value.key, aesKey, iv, plaintext)
      : encryptWithForge(publicKey.value.key, aesKey, iv, plaintext)
  return {
    encrypted_key: bytesToBase64Url(encryptedKey),
    fields: {
      [fieldName]: {
        iv: bytesToBase64Url(iv),
        value: bytesToBase64Url(encrypted),
      },
    },
  }
}

async function encryptWithWebCrypto(
  key: CryptoKey,
  aesKey: Uint8Array,
  iv: Uint8Array,
  plaintext: Uint8Array,
) {
  const cryptoKey = await crypto.subtle.importKey('raw', toArrayBuffer(aesKey), 'AES-GCM', false, [
    'encrypt',
  ])
  const encryptedKey = await crypto.subtle.encrypt(
    { name: 'RSA-OAEP' },
    key,
    toArrayBuffer(aesKey),
  )
  const encrypted = await crypto.subtle.encrypt(
    { name: 'AES-GCM', iv: toArrayBuffer(iv) },
    cryptoKey,
    toArrayBuffer(plaintext),
  )
  return {
    encryptedKey: new Uint8Array(encryptedKey),
    encrypted: new Uint8Array(encrypted),
  }
}

function encryptWithForge(
  key: forge.pki.rsa.PublicKey,
  aesKey: Uint8Array,
  iv: Uint8Array,
  plaintext: Uint8Array,
) {
  const encryptedKey = key.encrypt(bytesToBinary(aesKey), 'RSA-OAEP', {
    md: forge.md.sha256.create(),
    mgf1: {
      md: forge.md.sha256.create(),
    },
  })
  const cipher = forge.cipher.createCipher('AES-GCM', bytesToBinary(aesKey))
  cipher.start({ iv: bytesToBinary(iv), tagLength: 128 })
  cipher.update(forge.util.createBuffer(bytesToBinary(plaintext)))
  if (!cipher.finish()) throw new Error('加密请求失败')
  return {
    encryptedKey: binaryToBytes(encryptedKey),
    encrypted: binaryToBytes(cipher.output.getBytes() + cipher.mode.tag.getBytes()),
  }
}

function randomFieldName() {
  return bytesToBase64Url(randomBytes(12))
}

function hasWebCrypto() {
  return Boolean(
    typeof window !== 'undefined' &&
      window.isSecureContext &&
      globalThis.crypto?.subtle &&
      globalThis.crypto?.getRandomValues,
  )
}

function randomBytes(length: number) {
  const bytes = new Uint8Array(length)
  if (globalThis.crypto?.getRandomValues) return globalThis.crypto.getRandomValues(bytes)
  const random = forge.random.getBytesSync(length)
  for (let index = 0; index < random.length; index += 1) bytes[index] = random.charCodeAt(index)
  return bytes
}

function pemToArrayBuffer(pem: string) {
  const base64 = pem
    .replace('-----BEGIN PUBLIC KEY-----', '')
    .replace('-----END PUBLIC KEY-----', '')
    .replace(/\s/g, '')
  const binary = atob(base64)
  const bytes = new Uint8Array(binary.length)
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index)
  return bytes.buffer
}

function bytesToBase64Url(bytes: Uint8Array) {
  let binary = ''
  bytes.forEach((byte) => {
    binary += String.fromCharCode(byte)
  })
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '')
}

function bytesToBinary(bytes: Uint8Array) {
  let binary = ''
  bytes.forEach((byte) => {
    binary += String.fromCharCode(byte)
  })
  return binary
}

function binaryToBytes(binary: string) {
  const bytes = new Uint8Array(binary.length)
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index)
  return bytes
}

function toArrayBuffer(bytes: Uint8Array) {
  const buffer = new ArrayBuffer(bytes.byteLength)
  new Uint8Array(buffer).set(bytes)
  return buffer
}

function emptyToNull(value: string | null) {
  const trimmed = value?.trim() ?? ''
  return trimmed ? trimmed : null
}

function formatTicks(ticks: number | null) {
  if (!ticks || ticks < 0) return '--:--'
  const seconds = Math.floor(ticks / 10_000_000)
  const minutes = Math.floor(seconds / 60)
  const hours = Math.floor(minutes / 60)
  const mm = String(minutes % 60).padStart(2, '0')
  const ss = String(seconds % 60).padStart(2, '0')
  return hours > 0 ? `${hours}:${mm}:${ss}` : `${minutes}:${ss}`
}

function formatBytes(bytes: number | undefined) {
  if (!bytes) return '--'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value >= 10 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`
}

function formatUptime(seconds: number | undefined) {
  if (!seconds) return '--'
  const days = Math.floor(seconds / 86400)
  const hours = Math.floor((seconds % 86400) / 3600)
  return days > 0 ? `${days}天${hours}小时` : `${hours}小时`
}

function formatTimestamp(value: string) {
  const timestamp = Number(value)
  if (!Number.isFinite(timestamp) || timestamp <= 0) return '--'
  return new Date(timestamp * 1000).toLocaleString()
}

function formatLogTime(value: number) {
  if (!Number.isFinite(value) || value <= 0) return '--'
  return new Date(value).toLocaleString()
}

function formatTimestampMs(value: number | null | undefined) {
  if (!value || !Number.isFinite(value)) return '--'
  return new Date(value).toLocaleString()
}

function formatServerName(serverId: string) {
  return settings.servers.find((server) => server.id === serverId)?.name || serverId || '--'
}

function proxyStatusLabel(status: ProxyStatus | undefined) {
  if (!status) return '未启动'
  if (!status.enabled) return '未启用'
  return status.listening ? '监听中' : '未监听'
}

function validationClass(result: ValidationResult) {
  return result.ok ? 'success' : 'warn'
}

function logLevelLabel(level: ActivityLogEntry['level']) {
  if (level === 'success') return '成功'
  if (level === 'error') return '错误'
  if (level === 'warn') return '警告'
  return '信息'
}

function clientKeyword(record: ClientRuleRecord) {
  return record.user_agent || record.client_name || '--'
}

onMounted(bootstrap)
onBeforeUnmount(stopDashboardPolling)
</script>

<template>
  <main v-if="mode === 'loading'" class="auth-shell">
    <section class="auth-card">加载中</section>
  </main>

  <main v-else-if="mode === 'setup' || mode === 'login'" class="auth-shell">
    <section class="auth-card">
      <div class="brand-row">
        <div class="logo-mark">E</div>
        <div>
          <h1>Emby Panel</h1>
          <p>{{ mode === 'setup' ? '首次初始化' : '管理员登录' }}</p>
        </div>
      </div>
      <div v-if="error" class="notice error">{{ error }}</div>
      <label>
        <span>用户名</span>
        <input v-model="credentials.username" autocomplete="username" />
      </label>
      <label>
        <span>密码</span>
        <input
          v-model="credentials.password"
          type="password"
          autocomplete="current-password"
          @keyup.enter="mode === 'setup' ? setupAdmin() : login()"
        />
      </label>
      <button class="primary wide" :disabled="saving" @click="mode === 'setup' ? setupAdmin() : login()">
        {{ saving ? '处理中' : mode === 'setup' ? '创建并进入' : '登录' }}
      </button>
    </section>
  </main>

  <main v-else class="app-shell" :class="{ dark: darkMode }">
    <aside class="sidebar">
      <div class="brand-row compact">
        <div class="logo-mark">E</div>
        <div>
          <strong>{{ appInfo.name }}</strong>
          <small>{{ appInfo.version || '版本读取中' }}</small>
        </div>
      </div>

      <nav>
        <button
          v-for="item in menu"
          :key="item.id"
          class="nav-item"
          :class="{ active: page === item.id }"
          @click="setPage(item.id)"
        >
          <span>{{ item.icon }}</span>
          {{ item.label }}
        </button>
      </nav>

      <button class="nav-item logout" @click="logout">退出登录</button>
    </aside>

    <section class="workspace">
      <header class="topbar">
        <button
          class="icon-button theme-toggle"
          :aria-label="darkMode ? '切换到浅色模式' : '切换到暗夜模式'"
          :title="darkMode ? '浅色模式' : '暗夜模式'"
          @click="toggleTheme"
        >
          {{ darkMode ? '☾' : '☼' }}
        </button>
        <div class="top-actions">
          <button class="avatar" @click="setPage('account')">{{ profile.username.slice(0, 1) || 'A' }}</button>
        </div>
      </header>

      <div class="content">
        <div v-if="error" class="notice error">{{ error }}</div>
        <div v-if="notice" class="notice success">{{ notice }}</div>

        <section v-if="page === 'home'" class="dashboard">
          <section class="panel media-overview">
            <div class="panel-head">
              <h2>媒体库总览</h2>
              <span class="status-dot">在线 {{ mediaOverviews.length }}</span>
            </div>
            <div v-if="mediaOverviews.length" class="stat-grid four">
              <div class="stat-card">
                <span>电影</span>
                <strong>{{ mediaOverviewTotals.movie_count.toLocaleString() }}</strong>
                <small>Movies</small>
              </div>
              <div class="stat-card">
                <span>剧集</span>
                <strong>{{ mediaOverviewTotals.series_count.toLocaleString() }}</strong>
                <small>Series</small>
              </div>
              <div class="stat-card">
                <span>总集数</span>
                <strong>{{ mediaOverviewTotals.episode_count.toLocaleString() }}</strong>
                <small>Episodes</small>
              </div>
              <div class="stat-card">
                <span>用户</span>
                <strong>{{ mediaOverviewTotals.user_count.toLocaleString() }}</strong>
                <small>Users</small>
              </div>
            </div>
            <div v-else class="empty-state">{{ overviewError || '正在读取媒体库总览' }}</div>
            <div v-if="mediaOverviews.length" class="overview-server-list">
              <div v-for="overview in mediaOverviews" :key="overview.server_name" class="overview-server-row">
                <strong>{{ overview.server_name }}</strong>
                <span>电影 {{ overview.movie_count.toLocaleString() }}</span>
                <span>剧集 {{ overview.series_count.toLocaleString() }}</span>
                <span>集数 {{ overview.episode_count.toLocaleString() }}</span>
                <span>用户 {{ overview.user_count.toLocaleString() }}</span>
                <small>Emby {{ overview.version }} · {{ overview.operating_system }} · {{ overview.library_count }} 个媒体库</small>
              </div>
            </div>
          </section>

          <section class="panel health-panel">
            <div class="panel-head">
              <div>
                <h2>服务器状态</h2>
                <small class="health-subtitle">运行 {{ formatUptime(serverHealth?.uptime_seconds) }}</small>
              </div>
            </div>
            <div v-if="serverHealth" class="health-lines">
              <div class="health-line">
                <div>
                  <strong>CPU</strong>
                  <span>{{ serverHealth.cpu_name }} · {{ serverHealth.cpu_cores }} 核</span>
                </div>
                <b>{{ serverHealth.cpu_percent }}%</b>
                <div class="health-track">
                  <div class="health-bar cpu" :style="{ width: `${serverHealth.cpu_percent}%` }" />
                </div>
              </div>
              <div class="health-line">
                <div>
                  <strong>内存</strong>
                  <span>{{ formatBytes(serverHealth.memory_used_bytes) }} / {{ formatBytes(serverHealth.memory_total_bytes) }}</span>
                </div>
                <b>{{ serverHealth.memory_percent }}%</b>
                <div class="health-track">
                  <div class="health-bar memory" :style="{ width: `${serverHealth.memory_percent}%` }" />
                </div>
              </div>
              <div class="health-line">
                <div>
                  <strong>磁盘</strong>
                  <span>{{ formatBytes(serverHealth.disk_used_bytes) }} / {{ formatBytes(serverHealth.disk_total_bytes) }}</span>
                </div>
                <b>{{ serverHealth.disk_percent }}%</b>
                <div class="health-track">
                  <div class="health-bar disk" :style="{ width: `${serverHealth.disk_percent}%` }" />
                </div>
              </div>
            </div>
            <div v-else class="empty-state">{{ healthError || '正在读取服务器状态' }}</div>
          </section>

          <section class="panel operations-panel">
            <div class="panel-head">
              <div>
                <h2>运维概览</h2>
                <p class="muted">健康检查、反代监听和今日请求统计。</p>
              </div>
              <button class="secondary" @click="refreshOperationalData">刷新</button>
            </div>
            <div class="stat-grid four">
              <div class="stat-card">
                <span>今日请求</span>
                <strong>{{ requestStatsTotals.requests.toLocaleString() }}</strong>
                <small>{{ detailedHealth?.status === 'ok' ? '健康' : '待检查' }}</small>
              </div>
              <div class="stat-card">
                <span>重定向</span>
                <strong>{{ requestStatsTotals.redirects.toLocaleString() }}</strong>
                <small>STRM 直链</small>
              </div>
              <div class="stat-card">
                <span>缓存命中</span>
                <strong>{{ requestStatsTotals.cache_hits.toLocaleString() }}</strong>
                <small>内存直链缓存</small>
              </div>
              <div class="stat-card">
                <span>拦截 / 错误</span>
                <strong>{{ requestStatsTotals.blocks.toLocaleString() }} / {{ requestStatsTotals.errors.toLocaleString() }}</strong>
                <small>今日累计</small>
              </div>
            </div>
            <div v-if="updateCheck?.has_update" class="notice warn update-notice">
              发现新版本 {{ updateCheck.latest_version }}，当前 {{ updateCheck.current_version }}。
              <a :href="updateCheck.release_url" target="_blank" rel="noreferrer">查看 Release</a>
            </div>
            <div class="proxy-status-list">
              <div v-for="status in proxyStatuses" :key="status.server_id" class="proxy-status-row">
                <strong>{{ status.server_name }}</strong>
                <span>:{{ status.port }}</span>
                <span :class="['client-badge', status.listening ? 'allowed' : 'blocked']">
                  {{ proxyStatusLabel(status) }}
                </span>
                <small>最近请求 {{ formatTimestampMs(status.last_request_ms) }}</small>
              </div>
            </div>
          </section>

          <section class="panel rate-limit-overview">
            <div class="panel-head">
              <div>
                <h2>播放频率限制</h2>
                <p class="muted">首页直接查看当前窗口命中和封禁情况。</p>
              </div>
              <button class="secondary" @click="refreshRateLimitStatus">刷新</button>
            </div>
            <div class="stat-grid three">
              <div class="stat-card">
                <span>活跃窗口</span>
                <strong>{{ rateLimitOverview.active_windows.toLocaleString() }}</strong>
                <small>当前监控中的 IP</small>
              </div>
              <div class="stat-card">
                <span>已封禁</span>
                <strong>{{ rateLimitOverview.blocked_windows.toLocaleString() }}</strong>
                <small>屏蔽 IP / 禁用用户</small>
              </div>
              <div class="stat-card">
                <span>最高命中</span>
                <strong>{{ rateLimitOverview.highest_count.toLocaleString() }}</strong>
                <small>当前窗口最大次数</small>
              </div>
            </div>
            <div v-if="rateLimitWindows.length" class="rate-window-mini-list">
              <div v-for="row in rateLimitWindows.slice(0, 5)" :key="`home-${row.server_id}-${row.ip}`" class="rate-window-mini-row">
                <strong>{{ formatServerName(row.server_id) }}</strong>
                <span>{{ row.ip }}</span>
                <span>{{ row.current_count }}/{{ row.threshold }}</span>
                <span>{{ row.window_seconds }}s</span>
                <span :class="['client-badge', row.blocked ? 'blocked' : 'allowed']">
                  {{ row.blocked ? '已封禁' : '观察中' }}
                </span>
              </div>
            </div>
            <div v-else class="empty-state compact">当前没有播放频率窗口数据。</div>
          </section>

          <section class="panel playing-panel">
            <div class="panel-head">
              <h2>实时播放</h2>
              <div class="panel-actions">
                <span class="status-dot">在线 {{ activePlayCount }}</span>
                <button class="secondary" :disabled="playbackLoading" @click="refreshPlaybackSessions">
                  {{ playbackLoading ? '刷新中' : '刷新' }}
                </button>
              </div>
            </div>
            <div v-if="playbackSessions.length" class="playback-strip">
              <article v-for="session in playbackSessions" :key="session.id" class="play-card">
                <div class="poster-fallback">{{ session.item_name.slice(0, 1) }}</div>
                <div class="play-info">
                  <div class="play-title">{{ session.item_name }}</div>
                  <div class="play-meta">
                    {{ session.series_name || session.client }} · {{ session.user_name }} · {{ session.device_name }}
                  </div>
                  <div class="progress-track">
                    <div class="progress-bar" :style="{ width: `${session.percent ?? 0}%` }" />
                  </div>
                  <div class="play-footer">
                    <span>{{ formatTicks(session.position_ticks) }} / {{ formatTicks(session.runtime_ticks) }}</span>
                    <span>{{ session.transcoding ? '转码' : session.play_method || '直放' }}</span>
                  </div>
                </div>
              </article>
            </div>
            <div v-else class="empty-state">
              {{ playbackLoading ? '正在读取 Emby 播放会话' : playbackError || '当前没有正在播放的媒体' }}
            </div>
          </section>
        </section>

        <section v-else-if="page === 'server'" class="panel">
          <div class="panel-head">
            <div>
              <h2>服务器配置</h2>
              <p class="muted">每个 Emby 服务器使用独立反代端口，保存后自动监听。</p>
            </div>
            <div class="panel-actions">
              <button class="secondary" @click="addServer">添加服务器</button>
              <button class="secondary" :disabled="saving" @click="validateSettings">测试配置</button>
              <button class="primary" :disabled="saving" @click="saveSettings">
                {{ saving ? '保存中' : '保存配置' }}
              </button>
            </div>
          </div>
          <div class="server-list">
            <article v-for="(server, index) in settings.servers" :key="server.id" class="server-card">
              <div class="server-card-head">
                <strong>{{ server.name || `服务器 ${index + 1}` }}</strong>
                <div class="server-actions">
                  <label class="check compact-check">
                    <input v-model="server.enabled" type="checkbox" />
                    <span>启用</span>
                  </label>
                  <button
                    type="button"
                    class="secondary restart-button"
                    :disabled="saving || restartingServerId === server.id || !server.enabled"
                    @click="restartProxyServer(server)"
                  >
                    {{ restartingServerId === server.id ? '重启中' : '重启' }}
                  </button>
                  <button class="danger-button" :disabled="settings.servers.length <= 1" @click="removeServer(server.id)">
                    删除
                  </button>
                </div>
              </div>
              <div class="server-status-strip">
                <span :class="['client-badge', proxyStatusById[server.id]?.listening ? 'allowed' : 'blocked']">
                  {{ proxyStatusLabel(proxyStatusById[server.id]) }}
                </span>
                <span>端口 :{{ proxyStatusById[server.id]?.port || server.port }}</span>
                <span>启动 {{ formatTimestampMs(proxyStatusById[server.id]?.started_at_ms) }}</span>
                <span>最近请求 {{ formatTimestampMs(proxyStatusById[server.id]?.last_request_ms) }}</span>
                <span v-if="proxyStatusById[server.id]?.last_error" class="server-status-error">
                  {{ proxyStatusById[server.id]?.last_error }}
                </span>
              </div>
              <div class="grid server-grid">
                <label>
                  <span>名称</span>
                  <input v-model="server.name" placeholder="例如：主服务器" />
                </label>
                <label>
                  <span>Emby 地址</span>
                  <input v-model="server.emby_host" placeholder="http://emby.local:8096" />
                </label>
                <label>
                  <span>Emby API Key</span>
                  <div class="secret-input">
                    <input
                      v-model="server.emby_api_key"
                      :type="isApiKeyVisible(server.id) ? 'text' : 'password'"
                      autocomplete="off"
                    />
                    <button
                      type="button"
                      class="secret-toggle"
                      :aria-pressed="isApiKeyVisible(server.id)"
                      :aria-label="isApiKeyVisible(server.id) ? '隐藏 Emby API Key' : '显示 Emby API Key'"
                      :title="isApiKeyVisible(server.id) ? '隐藏 Emby API Key' : '显示 Emby API Key'"
                      @click="toggleApiKeyVisible(server.id)"
                    >
                      <span :class="['eye-icon', { off: !isApiKeyVisible(server.id) }]" aria-hidden="true" />
                    </button>
                  </div>
                </label>
                <label>
                  <span>反代端口</span>
                  <input v-model.number="server.port" type="number" min="1" max="65535" />
                </label>
              </div>
              <div class="grid real-ip-grid">
                <label>
                  <span>真实 IP 获取方式</span>
                  <select v-model="server.real_ip_mode" @change="updateRealIpMode(server)">
                    <option v-for="option in realIpModeOptions" :key="option.value" :value="option.value">
                      {{ option.label }}
                    </option>
                  </select>
                  <small v-if="server.real_ip_mode === 'header_list'" class="field-help">
                    从下列常用 CDN 携带真实 IP 的 HTTP Header 中获取，按顺序取第一个能获取到的值。
                  </small>
                </label>
                <label v-if="needsRealIpHeader(server)">
                  <span>{{ server.real_ip_mode === 'header' ? 'HTTP Header' : 'CDN Headers' }}</span>
                  <textarea
                    v-model="server.real_ip_header"
                    :placeholder="
                      server.real_ip_mode === 'header'
                        ? '例如：x-real-ip'
                        : defaultCdnHeaders
                    "
                  />
                </label>
              </div>
              <p class="muted real-ip-help">
                默认使用系统识别。经过 CDN 或多层反代后 IP 不准时再配置，保存后会同步重启对应反代服务。
              </p>
            </article>
          </div>

          <div class="grid common-grid">
            <label>
              <span>缓存秒数</span>
              <input v-model.number="settings.cache_ttl_seconds" type="number" min="0" />
            </label>
            <label>
              <span>缓存最大条数</span>
              <input v-model.number="settings.cache_max_capacity" type="number" min="1" />
            </label>
            <label>
              <span>OpenList 地址</span>
              <input v-model="settings.openlist_addr" placeholder="可选：http://openlist.local:5244" />
            </label>
            <label>
              <span>OpenList Token</span>
              <input v-model="settings.openlist_token" type="password" placeholder="可选" />
            </label>
          </div>
          <div class="grid cache-filter-grid">
            <label>
              <span>缓存过滤模式</span>
              <select v-model="settings.cache_domain_filter_mode">
                <option value="off">不过滤</option>
                <option value="whitelist">白名单：命中才缓存</option>
                <option value="blacklist">黑名单：命中不缓存</option>
              </select>
            </label>
            <label>
              <span>缓存过滤域名</span>
              <textarea
                v-model="settings.cache_domain_whitelist"
                :disabled="settings.cache_domain_filter_mode === 'off'"
                placeholder="支持多个域名、通配符或关键字；每行一个，例如：*.115cdn.* 或 115"
              />
            </label>
          </div>
          <p class="muted cache-filter-help">
            缓存过滤只匹配直链域名部分。白名单模式下命中才缓存；黑名单模式下命中不缓存，其他直链正常缓存。
          </p>
          <div class="row">
            <label class="check">
              <input v-model="settings.enable_internal_redirect" type="checkbox" />
              <span>开启内部重定向 HEAD 解析</span>
            </label>
            <label class="small">
              <span>HEAD 超时秒数</span>
              <input v-model.number="settings.internal_redirect_timeout_seconds" type="number" min="1" />
            </label>
          </div>
          <label class="block">
            <span>STRM URL 映射</span>
            <textarea
              v-model="settings.strm_url_mappings"
              spellcheck="false"
              placeholder="每行一个映射：原地址 => 新地址&#10;https://source.example.com => http://media-gateway.local:5244&#10;高级正则：regex:https://source\\.(example|test)\\.com => http://media-gateway.local:5244"
            />
          </label>

          <section class="config-tools single">
            <div class="tool-block">
              <div class="panel-head compact">
                <h3>配置测试结果</h3>
                <button class="secondary" :disabled="saving" @click="validateSettings">重新测试</button>
              </div>
              <div v-if="validationResults.length" class="validation-list">
                <div
                  v-for="result in validationResults"
                  :key="`${result.scope}-${result.message}-${result.detail}`"
                  :class="['validation-row', validationClass(result)]"
                >
                  <strong>{{ result.scope }}</strong>
                  <span>{{ result.message }}</span>
                  <small>{{ result.detail || '--' }}</small>
                </div>
              </div>
              <div v-else class="empty-state compact">还没有运行配置测试。</div>
            </div>
          </section>
        </section>

        <section v-else-if="page === 'clients'" class="client-page">
          <section class="panel">
            <div class="panel-head">
              <div>
                <h2>客户端管控</h2>
                <p class="muted">自动记录播放设备和 UA，也可以按播放频率临时禁用账号。</p>
              </div>
              <div class="panel-actions">
                <button class="secondary" @click="clientControl.enabled = !clientControl.enabled">
                  {{ clientControl.enabled ? '已启用' : '未启用' }}
                </button>
                <button class="secondary" @click="refreshClientControl">刷新</button>
                <button class="primary" :disabled="savingClientControl" @click="saveClientControl">
                  {{ savingClientControl ? '保存中' : '保存' }}
                </button>
              </div>
            </div>
            <div v-if="clientControlError" class="notice error">{{ clientControlError }}</div>
            <div class="client-toolbar">
              <label class="check">
                <input v-model="clientControl.enabled" type="checkbox" />
                <span>启用 UA 拦截</span>
              </label>
              <label class="check">
                <input v-model="clientControl.playback_rate_limit_enabled" type="checkbox" />
                <span>启用播放频率限制</span>
              </label>
              <span class="client-count">已记录 {{ clientControl.records.length }} 个客户端</span>
            </div>
            <div class="rate-limit-grid">
              <label>
                <span>屏蔽方式</span>
                <select v-model="clientControl.playback_rate_limit_action">
                  <option v-for="option in playbackLimitActionOptions" :key="option.value" :value="option.value">
                    {{ option.label }}
                  </option>
                </select>
                <small class="field-help">
                  {{
                    playbackLimitActionOptions.find((option) => option.value === clientControl.playback_rate_limit_action)?.description
                  }}
                </small>
              </label>
              <label>
                <span>检测时间窗口（秒）</span>
                <input v-model.number="clientControl.playback_rate_limit_window_seconds" type="number" min="1" />
              </label>
              <label>
                <span>最大播放次数</span>
                <input v-model.number="clientControl.playback_rate_limit_max_requests" type="number" min="1" />
              </label>
              <label>
                <span>{{ clientControl.playback_rate_limit_action === 'block_ip' ? '屏蔽时长（秒）' : '重复拦截冷却（秒）' }}</span>
                <input v-model.number="clientControl.playback_rate_limit_block_seconds" type="number" min="1" />
              </label>
            </div>
            <p class="muted rate-limit-help">
              同一 IP 在窗口内超过次数后，按选择的方式处理：屏蔽 IP 为临时封禁；禁用用户会调用 Emby API 禁用账号。
            </p>
            <div class="rate-block-list">
              <div class="rate-block-head">
                <strong>当前封禁</strong>
                <span>{{ activeRateLimitBlocks.length }} 条</span>
              </div>
              <div v-if="activeRateLimitBlocks.length" class="rate-block-table-wrap">
                <table class="rate-block-table">
                  <thead>
                    <tr>
                      <th>方式</th>
                      <th>服务器</th>
                      <th>IP</th>
                      <th>用户</th>
                      <th>到期时间</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    <tr v-for="record in activeRateLimitBlocks" :key="record.id">
                      <td>{{ record.action === 'disable_user' ? '禁用用户' : '屏蔽 IP' }}</td>
                      <td>{{ record.server_name }}</td>
                      <td>
                        <strong>{{ rateLimitBlockIp(record) }}</strong>
                      </td>
                      <td>{{ record.user_name || '--' }}</td>
                      <td>{{ formatTimestamp(record.blocked_until) }}</td>
                      <td>
                        <button class="secondary" @click="unblockRateLimit(record)">解除封禁</button>
                      </td>
                    </tr>
                  </tbody>
                </table>
              </div>
              <div v-else class="empty-state compact">暂无频率限制封禁。</div>
            </div>
            <div class="client-filterbar">
              <button
                :class="['filter-button', { active: clientStatusFilter === 'all' }]"
                @click="clientStatusFilter = 'all'"
              >
                全部 {{ clientControl.records.length }}
              </button>
              <button
                :class="['filter-button', { active: clientStatusFilter === 'blocked' }]"
                @click="clientStatusFilter = 'blocked'"
              >
                已禁用 {{ blockedClientCount }}
              </button>
              <button
                :class="['filter-button', { active: clientStatusFilter === 'allowed' }]"
                @click="clientStatusFilter = 'allowed'"
              >
                允许播放 {{ allowedClientCount }}
              </button>
              <input
                v-model="clientKeywordFilter"
                class="client-search"
                placeholder="搜索 UA / 客户端 / 设备 / 用户 / 描述"
              />
              <button
                class="secondary"
                :disabled="clientStatusFilter === 'all' && !clientKeywordFilter"
                @click="clearClientFilters"
              >
                清空筛选
              </button>
            </div>
          </section>

          <section class="panel">
            <div class="panel-head">
              <div>
                <h2>播放频率窗口</h2>
                <p class="muted">显示当前检测窗口内各 IP 的播放请求计数。</p>
              </div>
              <button class="secondary" @click="refreshRateLimitStatus">刷新</button>
            </div>
            <div v-if="rateLimitWindows.length" class="rate-window-table-wrap">
              <table class="rate-window-table">
                <thead>
                  <tr>
                    <th>服务器</th>
                    <th>IP</th>
                    <th>当前次数</th>
                    <th>阈值</th>
                    <th>剩余</th>
                    <th>窗口</th>
                    <th>重置时间</th>
                    <th>状态</th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="row in rateLimitWindows" :key="`${row.server_id}-${row.ip}`">
                    <td>{{ formatServerName(row.server_id) }}</td>
                    <td>{{ row.ip }}</td>
                    <td>{{ row.current_count }}</td>
                    <td>{{ row.threshold }}</td>
                    <td>{{ row.remaining }}</td>
                    <td>{{ row.window_seconds }}s</td>
                    <td>{{ formatTimestamp(row.reset_at) }}</td>
                    <td>
                      <span :class="['client-badge', row.blocked ? 'blocked' : 'allowed']">
                        {{ row.blocked ? '已封禁' : '观察中' }}
                      </span>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
            <div v-else class="empty-state compact">当前没有播放频率窗口数据。</div>
          </section>

          <section class="panel">
            <div class="panel-head">
              <div>
                <h2>手动添加 UA 拦截</h2>
                <p class="muted">输入 UA 完整内容或关键字，保存后默认进入禁用状态。</p>
              </div>
            </div>
            <div class="manual-rule-row">
              <label>
                <span>UA 关键字</span>
                <input
                  v-model="manualClientRule.user_agent"
                  placeholder="例如：Infuse / Fileball / okhttp"
                  @keyup.enter="addClientRule"
                />
              </label>
              <label>
                <span>描述</span>
                <input v-model="manualClientRule.note" placeholder="例如：临时禁用某客户端" @keyup.enter="addClientRule" />
              </label>
              <button class="primary" :disabled="addingClientRule" @click="addClientRule">
                {{ addingClientRule ? '添加中' : '添加拦截' }}
              </button>
            </div>
          </section>

          <section class="panel client-table-panel">
            <div class="client-table-wrap">
              <table class="client-table">
                <thead>
                  <tr>
                    <th>关键字</th>
                    <th>描述</th>
                    <th>状态</th>
                    <th>创建时间</th>
                    <th>更新时间</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="record in clientRuleRows" :key="record.id">
                    <td>
                      <strong>{{ clientKeyword(record) }}</strong>
                      <small>{{ record.client_name }} · {{ record.device_name }} · {{ record.user_name }}</small>
                    </td>
                    <td>
                      <span>{{ record.note || (record.source === 'auto' ? '自动记录播放设备' : '手动 UA 拦截') }}</span>
                      <small>{{ record.source === 'auto' ? '自动记录' : '手动添加' }}</small>
                    </td>
                    <td>
                      <span :class="['client-badge', record.enabled ? 'blocked' : 'allowed']">
                        {{ record.enabled ? '已禁用' : '允许播放' }}
                      </span>
                    </td>
                    <td>{{ formatTimestamp(record.created_at) }}</td>
                    <td>{{ formatTimestamp(record.updated_at) }}</td>
                    <td>
                      <div class="rule-actions">
                        <button
                          type="button"
                          :class="['switch-button', { active: record.enabled }]"
                          :aria-pressed="record.enabled"
                          :aria-label="record.enabled ? '关闭 UA 拦截' : '开启 UA 拦截'"
                          @click="toggleClientRule(record)"
                        >
                          <span />
                        </button>
                        <button type="button" class="danger-button" @click="deleteClientRule(record)">
                          删除
                        </button>
                      </div>
                    </td>
                  </tr>
                </tbody>
              </table>
              <div v-if="!clientRuleRows.length" class="empty-state">
                {{ clientControl.records.length ? '当前筛选没有匹配的客户端。' : '暂无客户端记录，开始播放后会自动出现，也可以手动添加 UA 拦截。' }}
              </div>
            </div>
          </section>
        </section>

        <section v-else-if="page === 'notifications'" class="client-page">
          <section class="panel webhook-panel">
            <div class="panel-head">
              <div>
                <h2>通知配置</h2>
                <p class="muted">Webhook 使用 POST JSON 发送：{ title, text }。命中通知包含播放频率屏蔽和 UA 拦截命中。</p>
              </div>
              <div class="panel-actions">
                <label class="check compact-check">
                  <input v-model="clientControl.notify_enabled" type="checkbox" />
                  <span>命中通知</span>
                </label>
                <button class="secondary" @click="addWebhook">添加 Webhook</button>
                <button class="primary" :disabled="savingClientControl" @click="saveClientControl">
                  {{ savingClientControl ? '保存中' : '保存通知' }}
                </button>
              </div>
            </div>
            <div v-if="clientControlError" class="notice error">{{ clientControlError }}</div>
            <div class="webhook-list">
              <article v-for="(webhook, index) in clientControl.webhooks" :key="webhook.id" class="webhook-item">
                <div class="webhook-item-head">
                  <label class="check compact-check">
                    <input v-model="webhook.enabled" type="checkbox" />
                    <span>启用</span>
                  </label>
                  <div class="rule-actions">
                    <button class="secondary" :disabled="testingWebhook" @click="testWebhook(webhook)">
                      {{ testingWebhook ? '测试中' : '测试连接' }}
                    </button>
                    <button class="danger-button" @click="removeWebhook(index)">删除</button>
                  </div>
                </div>
                <div class="grid webhook-grid">
                  <label>
                    <span>名称</span>
                    <input v-model="webhook.name" placeholder="例如：企业微信通知" />
                  </label>
                  <label>
                    <span>Webhook URL</span>
                    <input v-model="webhook.url" placeholder="https://example.com/webhook" />
                  </label>
                  <label>
                    <span>密钥（可选）</span>
                    <input v-model="webhook.secret" type="password" placeholder="可选密钥" />
                  </label>
                </div>
              </article>
            </div>
            <p class="muted rate-limit-help">请求体固定为 {"title":"${title}","text":"${text}"}，密钥会通过 `X-Webhook-Secret` 头发送。</p>
          </section>
        </section>

        <section v-else-if="page === 'backup'" class="backup-page">
          <section class="panel">
            <div class="panel-head">
              <div>
                <h2>配置备份</h2>
                <p class="muted">导出配置文件，或从电脑选择配置文件还原运行配置。</p>
              </div>
            </div>
            <input
              ref="backupFileInput"
              class="visually-hidden"
              type="file"
              accept=".json,application/json,text/plain"
              @change="handleBackupFileSelected"
            />
            <div v-if="backupError" class="notice error">{{ backupError }}</div>
            <div class="backup-layout">
              <section class="backup-card">
                <h3>备份范围</h3>
                <div class="backup-scope-grid">
                  <div>
                    <strong>服务器配置</strong>
                    <span>Emby 地址、API Key、反代端口、真实 IP、缓存和映射规则</span>
                  </div>
                  <div>
                    <strong>客户端管控</strong>
                    <span>UA 拦截、播放频率限制、封禁列表和客户端规则</span>
                  </div>
                  <div>
                    <strong>通知配置</strong>
                    <span>Webhook 地址、启用状态和密钥</span>
                  </div>
                  <div>
                    <strong>日志配置</strong>
                    <span>日志级别、文件大小、保留数量和格式</span>
                  </div>
                </div>
                <p class="muted backup-note">
                  不包含面板管理员用户名、密码、登录会话、运行日志文件和请求统计数据。
                </p>
              </section>

              <section class="backup-card">
                <h3>配置文件备份 / 还原</h3>
                <div class="backup-actions text-actions">
                  <button class="secondary" @click="exportBackup">备份</button>
                  <button class="primary" @click="importBackup">还原</button>
                </div>
                <div class="backup-drop-hint">
                  <strong>备份</strong>
                  <span>点击后会自动生成 `embypanel-config-时间.json` 并弹出浏览器下载。</span>
                  <strong>还原</strong>
                  <span>点击后选择本机配置文件，读取成功后自动还原并重启反代服务。</span>
                </div>
              </section>
            </div>
          </section>
        </section>

        <section v-else-if="page === 'logs'" class="logs-page">
          <section class="panel">
            <div class="panel-head">
              <div>
                <h2>可视化日志</h2>
                <p class="muted">按服务器查看播放日志和 info 级别运行信息，页面打开时每 3 秒自动刷新。</p>
              </div>
              <div class="panel-actions">
                <span class="status-dot">{{ logsLoading ? '刷新中' : '实时刷新' }}</span>
                <button class="secondary" :disabled="logsLoading" @click="refreshActivityLogs">
                  {{ logsLoading ? '刷新中' : '刷新' }}
                </button>
              </div>
            </div>
            <div v-if="logsError" class="notice error">{{ logsError }}</div>
            <div class="log-toolbar">
              <label>
                <span>服务器</span>
                <select v-model="selectedLogServer" @change="refreshActivityLogs">
                  <option value="all">全部服务器</option>
                  <option v-for="server in logServers" :key="server.id" :value="server.id">
                    {{ server.name }} · {{ server.enabled ? '启用' : '停用' }} · :{{ server.port }}
                  </option>
                </select>
              </label>
              <label>
                <span>日志类型</span>
                <select v-model="selectedLogKind" @change="refreshActivityLogs">
                  <option value="all">播放 + 信息</option>
                  <option value="playback">播放日志</option>
                  <option value="general">信息</option>
                </select>
              </label>
              <label>
                <span>日志级别</span>
                <select v-model="selectedLogLevel" @change="refreshActivityLogs">
                  <option value="all">全部级别</option>
                  <option value="success">SUCCESS - 成功</option>
                  <option value="info">INFO - 信息</option>
                  <option value="warn">WARNING - 警告</option>
                  <option value="error">ERROR - 错误</option>
                </select>
              </label>
              <label>
                <span>关键词</span>
                <input v-model="logKeywordFilter" placeholder="搜索用户名 / IP / URL / 信息" @keyup.enter="refreshActivityLogs" />
              </label>
              <label>
                <span>开始时间</span>
                <input v-model="logSince" type="datetime-local" @change="refreshActivityLogs" />
              </label>
              <label>
                <span>结束时间</span>
                <input v-model="logUntil" type="datetime-local" @change="refreshActivityLogs" />
              </label>
              <div class="log-filter-actions">
                <button class="secondary" @click="refreshActivityLogs">筛选</button>
                <button class="secondary" @click="exportLogs">导出 CSV</button>
              </div>
              <div class="log-summary">
                <div>
                  <span>播放日志</span>
                  <strong>{{ playbackLogRows.length }}</strong>
                </div>
                <div>
                  <span>信息</span>
                  <strong>{{ generalLogRows.length }}</strong>
                </div>
              </div>
            </div>
          </section>

          <section class="panel">
            <div class="panel-head">
              <div>
                <h2>日志文件配置</h2>
                <p class="muted">日志写入 data/logs/embypanel.log，默认 INFO 级别。</p>
              </div>
              <button class="primary" @click="saveLogConfig">保存日志配置</button>
            </div>
            <div class="grid log-config-grid">
              <label>
                <span>日志级别</span>
                <select v-model="logConfig.level">
                  <option value="debug">DEBUG - 调试</option>
                  <option value="info">INFO - 信息</option>
                  <option value="warning">WARNING - 警告</option>
                  <option value="error">ERROR - 错误</option>
                  <option value="critical">CRITICAL - 严重</option>
                </select>
              </label>
              <label>
                <span>单文件最大 MB</span>
                <input v-model.number="logConfig.max_size_mb" type="number" min="1" max="1024" />
              </label>
              <label>
                <span>保留文件数</span>
                <input v-model.number="logConfig.max_backups" type="number" min="1" max="99" />
              </label>
              <label class="log-format-field">
                <span>日志格式</span>
                <input v-model="logConfig.format" />
              </label>
            </div>
          </section>

          <section v-if="selectedLogKind !== 'general'" class="panel log-panel">
            <div class="panel-head">
              <h2>播放日志</h2>
              <span class="muted">{{ selectedLogServer === 'all' ? '全部服务器' : '单服务器' }}</span>
            </div>
            <div v-if="playbackLogRows.length" class="log-list">
              <article v-for="entry in playbackLogRows" :key="entry.id" :class="['log-entry', 'playback', entry.level, 'playback-detail']">
                <div class="log-body">
                  <div class="playback-log-meta">
                    <span class="server-pill">{{ entry.server_name }}</span>
                    <span class="user-pill">{{ entry.playback_user || '--' }}</span>
                    <span class="ip-pill">{{ entry.playback_ip || '--' }}</span>
                    <span class="log-full-time">{{ formatLogTime(entry.timestamp_ms) }}</span>
                    <strong>{{ entry.message }}</strong>
                  </div>
                  <p class="log-detail">{{ entry.detail || '暂无详情' }}</p>
                </div>
              </article>
            </div>
            <div v-else class="empty-state">暂无播放日志，开始播放后会自动出现。</div>
          </section>

          <section v-if="selectedLogKind !== 'playback'" class="panel log-panel">
            <div class="panel-head">
              <h2>信息</h2>
              <span class="muted">{{ selectedLogServer === 'all' ? '全部服务器' : '单服务器' }}</span>
            </div>
            <div v-if="generalLogRows.length" class="log-list">
              <article
                v-for="entry in generalLogRows"
                :key="entry.id"
                :class="['log-entry', 'general', entry.level]"
              >
                <div class="log-time">{{ formatLogTime(entry.timestamp_ms) }}</div>
                <div class="log-body">
                  <div class="log-title">
                    <strong>{{ entry.message }}</strong>
                    <span class="server-pill">{{ entry.server_name }}</span>
                    <span :class="['level-pill', entry.level]">{{ logLevelLabel(entry.level) }}</span>
                  </div>
                  <p>{{ entry.detail || '暂无详情' }}</p>
                </div>
              </article>
            </div>
            <div v-else class="empty-state">暂无 info 级别运行信息，服务启动或反代访问后会自动出现。</div>
          </section>
        </section>

        <section v-else class="account-grid">
          <section class="panel">
            <div class="panel-head">
              <h2>账户资料</h2>
              <button class="primary" :disabled="savingProfile" @click="saveProfile">
                {{ savingProfile ? '保存中' : '保存资料' }}
              </button>
            </div>
            <label>
              <span>用户名</span>
              <input v-model="profileForm.username" autocomplete="username" />
            </label>
          </section>

          <section class="panel">
            <div class="panel-head">
              <h2>修改密码</h2>
            </div>
            <div class="grid">
              <label>
                <span>当前密码</span>
                <input v-model="passwordForm.current_password" type="password" autocomplete="current-password" />
              </label>
              <label>
                <span>新密码</span>
                <input v-model="passwordForm.new_password" type="password" autocomplete="new-password" />
              </label>
              <label>
                <span>确认新密码</span>
                <input
                  v-model="passwordForm.confirm_password"
                  type="password"
                  autocomplete="new-password"
                  @keyup.enter="changePassword"
                />
              </label>
            </div>
            <div class="form-actions">
              <button class="primary" :disabled="changingPassword" @click="changePassword">
                {{ changingPassword ? '修改中' : '修改密码' }}
              </button>
            </div>
          </section>

          <section class="panel">
            <div class="panel-head">
              <div>
                <h2>配置审计</h2>
                <p class="muted">记录配置、账户、通知、备份恢复等管理操作，不保存敏感明文。</p>
              </div>
              <button class="secondary" @click="refreshAuditLogs">刷新</button>
            </div>
            <div class="audit-toolbar">
              <label>
                <span>操作类型</span>
                <select v-model="selectedAuditAction" @change="refreshAuditLogs">
                  <option v-for="action in auditActionOptions" :key="action" :value="action">
                    {{ action === 'all' ? '全部操作' : action }}
                  </option>
                </select>
              </label>
              <label>
                <span>关键词</span>
                <input v-model="auditKeywordFilter" placeholder="搜索管理员 / 操作 / 摘要" @keyup.enter="refreshAuditLogs" />
              </label>
            </div>
            <div v-if="auditLogs.length" class="audit-list">
              <article v-for="entry in auditLogs" :key="entry.id" class="audit-row">
                <div>
                  <strong>{{ entry.action }}</strong>
                  <span>{{ entry.summary }}</span>
                </div>
                <small>{{ entry.admin_username || '--' }} · {{ entry.result }} · {{ formatTimestampMs(entry.timestamp_ms) }}</small>
              </article>
            </div>
            <div v-else class="empty-state compact">暂无审计记录。</div>
          </section>
        </section>
      </div>
    </section>
  </main>
</template>
