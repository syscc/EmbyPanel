import { Archive, Bell, FileText, LayoutDashboard, Server, UserRound, Users } from '@lucide/vue'
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'

import { useRoute, useRouter } from 'vue-router'
import { useActionDialog } from '@/composables/useActionDialog'
import { useBackupController } from '@/composables/useBackupController'
import { usePayloadEncryption } from '@/composables/usePayloadEncryption'
import { createApiClient } from '@/lib/api-client'
import {
  emptyToNull,
  formatBytes,
  formatLogTime as formatLogTimeValue,
  formatTicks,
  formatTimestamp as formatTimestampValue,
  formatTimestampMs as formatTimestampMsValue,
  formatUptime as formatUptimeValue,
  isHttpUrl,
  requestOutcomeClass,
} from '@/lib/panel-formatters'
import { translations } from '@/lib/translations'
import type {
  Settings,
  RealIpMode,
  EmbyServerConfig,
  AppInfo,
  Profile,
  PlaybackSession,
  MediaOverview,
  MediaOverviewTotals,
  ServerHealth,
  DetailedHealth,
  ProxyStatus,
  ConnectivityCheckStatus,
  RequestStatsDaily,
  IpLocation,
  ProxyRequestDetail,
  UpdateCheck,
  ClientRuleRecord,
  PlaybackRateBlockRecord,
  PlaybackRateWindowStatus,
  WebhookNotifyConfig,
  ClientControlConfig,
  ActivityLogEntry,
  AuditLogEntry,
  ValidationResult,
  ValidationResponse,
  SystemLogConfig,
  AuthMode,
  Page,
  ClientStatusFilter,
  LogKindFilter,
  LogViewFilter,
  Locale,
} from '@/types/panel'

export function usePanelController() {
  const tokenKey = 'embypanel_token'
  const pageKey = 'embypanel_page'
  const themeKey = 'embypanel_theme'
  const localeKey = 'embypanel_locale'
  const validPages: Page[] = ['home', 'server', 'clients', 'users', 'logs', 'notifications', 'backup', 'account']
  const mode = ref<AuthMode>('loading')
  const route = useRoute()
  const router = useRouter()
  const { confirmAction, promptAction } = useActionDialog()
  const page = computed<Page>(() => {
    const routeName = route.name
    return typeof routeName === 'string' && validPages.includes(routeName as Page)
      ? routeName as Page
      : 'home'
  })
  const token = ref(storedToken())
  const darkMode = ref(storedTheme() === 'dark')
  const locale = ref<Locale>(storedLocale())
  const mobileNavOpen = ref(false)
  const mobileMenuButton = ref<HTMLButtonElement | null>(null)
  const mobileNavCloseButton = ref<HTMLButtonElement | null>(null)
  const mobileSidebar = ref<HTMLElement | null>(null)
  const saving = ref(false)
  const pageLoading = ref(false)
  const pageReady = ref(false)
  const restartingServerId = ref('')
  const savingAccount = ref(false)
  const error = ref('')
  const notice = ref('')
  const playbackSessions = ref<PlaybackSession[]>([])
  const playbackLoading = ref(false)
  const playbackError = ref('')
  const activityLogs = ref<ActivityLogEntry[]>([])
  const proxyRequestDetails = ref<ProxyRequestDetail[]>([])
  const logsLoading = ref(false)
  const logsError = ref('')
  const selectedLogServer = ref('all')
  const selectedLogView = ref<LogViewFilter>('playback')
  const selectedLogLevel = ref('all')
  const selectedRequestPathType = ref('all')
  const logKeywordFilter = ref('')
  const logSince = ref('')
  const logUntil = ref('')
  const playbackLogLimit = ref(120)
  const generalLogLimit = ref(80)
  const requestDetailLimit = ref(200)
  const mediaOverviews = ref<MediaOverview[]>([])
  const serverHealth = ref<ServerHealth | null>(null)
  const detailedHealth = ref<DetailedHealth | null>(null)
  const proxyStatuses = ref<ProxyStatus[]>([])
  const connectivityStatuses = ref<ConnectivityCheckStatus[]>([])
  const requestStats = ref<RequestStatsDaily[]>([])
  const updateCheck = ref<UpdateCheck | null>(null)
  const updateChecking = ref(false)
  const updateCheckError = ref('')
  const validationResults = ref<ValidationResult[]>([])
  const rateLimitWindows = ref<PlaybackRateWindowStatus[]>([])
  const auditLogs = ref<AuditLogEntry[]>([])
  const auditLogsError = ref('')
  const auditKeywordFilter = ref('')
  const selectedAuditAction = ref('all')
  const overviewError = ref('')
  const healthError = ref('')

  const api = createApiClient({
    getToken: () => token.value,
    onUnauthorized(path, requestToken) {
      if (path !== '/api/change-password' && passwordChangeToken === requestToken) {
        deferredAuthExpiredToken = requestToken
      } else {
        handleAuthExpired(requestToken)
      }
    },
  })
  const { encryptPayload, warmPublicKey, randomId } = usePayloadEncryption({ api, translate: t })

  const clientControl = reactive<ClientControlConfig>({
    enabled: false,
    notify_enabled: false,
    playback_rate_limit_enabled: false,
    playback_rate_limit_window_seconds: 60,
    playback_rate_limit_max_requests: 20,
    playback_rate_limit_block_seconds: 1800,
    playback_rate_limit_action: 'block_ip',
    concurrent_playback_limit_enabled: false,
    concurrent_playback_limit_max: 3,
    rate_limit_blocks: [],
    webhooks: [{
      id: newWebhookId(),
      enabled: false,
      name: 'Webhook',
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
  const revealedApiKeys = ref<Record<string, string>>({})
  const revealingApiKeyServers = ref<Record<string, boolean>>({})
  const expandedServerCards = ref<Record<string, boolean>>({})
  const logConfig = reactive<SystemLogConfig>({
    debug_mode: false,
    level: 'info',
    max_size_mb: 5,
    max_backups: 10,
    format: '[%(levelname)s] %(asctime)s - %(message)s',
  })
  let dashboardTimer: number | undefined
  let noticeTimer: number | undefined
  let pageActivation = 0
  let pagePollRunning = false
  let logRequestId = 0
  let auditRequestId = 0
  let dataSession = 0
  let apiKeyRevealGeneration = 0
  let persistedServerIds = new Set<string>()
  let accountSaveOperation = 0
  let passwordChangeToken = ''
  let deferredAuthExpiredToken = ''
  let logoutInProgress = false
  let settingsLoaded = false
  let clientControlLoaded = false
  let logConfigLoaded = false
  let settingsRequest: Promise<void> | undefined
  let clientControlRequest: Promise<void> | undefined
  let logConfigRequest: Promise<void> | undefined

  const credentials = reactive({
    username: '',
    password: '',
  })

  const logoUrl = `${import.meta.env.BASE_URL}logo.svg`

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
    cache_enabled: true,
    cache_domain_filter_mode: 'off',
    cache_domain_whitelist: '',
    enable_internal_redirect: false,
    internal_redirect_timeout_seconds: 15,
    strm_url_mappings: '',
    strm_url_mapping_enabled: true,
    connectivity_check_enabled: true,
    connectivity_check_interval_seconds: 60,
    connectivity_check_timeout_seconds: 5,
    connectivity_auto_restart_seconds: 180,
  })

  const {
    backupError,
    backupBusy,
    backupFileInput,
    exportBackup,
    importBackup,
    handleBackupFileSelected,
  } = useBackupController({
    api,
    encryptPayload,
    translate: t,
    showNotice,
    clearNotice,
    confirmAction,
    promptAction,
    onImported(response) {
      resetResourceCache()
      Object.assign(settings, response)
      normalizeSettingsServers()
      settingsLoaded = true
    },
  })

  const menu = [
    { id: 'home' as const, label: '首页', icon: LayoutDashboard },
    { id: 'server' as const, label: '服务器', icon: Server },
    { id: 'clients' as const, label: '客户端', icon: Users },
    { id: 'users' as const, label: '用户', icon: UserRound },
    { id: 'logs' as const, label: '日志', icon: FileText },
    { id: 'notifications' as const, label: '通知', icon: Bell },
    { id: 'backup' as const, label: '备份', icon: Archive },
    { id: 'account' as const, label: '账户', icon: UserRound },
  ]

  const activityLogMaxLimit = 800
  const requestDetailDisplayMax = 500
  const requestDetailPersistDays = 7
  const requestDetailPersistMax = 20000
  const playbackLogInitialLimit = 120
  const generalLogInitialLimit = 80
  const requestDetailInitialLimit = 200
  const activityLogLoadStep = 80
  const requestDetailLoadStep = 100
  const logViewLabels: Record<LogViewFilter, string> = {
    playback: '播放日志',
    blocked: '拦截日志',
    proxy: '反代请求',
    general: '运行日志',
  }

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
    { value: 'mixed', label: '混合模式', description: '同时禁用用户并屏蔽频繁播放的 IP' },
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
  const strmMappingPlaceholder = computed(() =>
    locale.value === 'en-US'
      ? 'One mapping per line: source => target\nhttps://source.example.com => http://media-gateway.local:5244\nRegex: regex:https://(source|mirror)\\.example\\.test => http://media-gateway.local:5244'
      : '每行一个映射：原地址 => 新地址\nhttps://source.example.com => http://media-gateway.local:5244\n高级正则：regex:https://(source|mirror)\\.example\\.test => http://media-gateway.local:5244',
  )
  const logServers = computed(() =>
    settings.servers.map((server) => ({
      id: server.id,
      name: server.name || `${t('端口')} ${server.port}`,
      port: server.port,
      enabled: server.enabled,
    })),
  )
  const playbackLogRows = computed(() => activityLogs.value.filter((entry) => entry.kind === 'playback'))
  const generalLogRows = computed(() =>
    activityLogs.value.filter((entry) => entry.kind === 'general'),
  )
  const requestDetailRows = computed(() => proxyRequestDetails.value)
  const selectedLogViewLabel = computed(() => t(logViewLabels[selectedLogView.value]))
  const visibleActivityLogRows = computed(() =>
    activityLogs.value.filter((entry) => selectedLogLevel.value === 'all' || entry.level === selectedLogLevel.value),
  )
  const canLoadMorePlaybackLogs = computed(() =>
    selectedLogView.value === 'playback'
      && playbackLogRows.value.length >= playbackLogLimit.value
      && playbackLogLimit.value < activityLogMaxLimit,
  )
  const canLoadMoreGeneralLogs = computed(() =>
    selectedLogView.value === 'general'
      && generalLogRows.value.length >= generalLogLimit.value
      && generalLogLimit.value < activityLogMaxLimit,
  )
  const canLoadMoreRequestDetails = computed(() =>
    isRequestDetailLogView.value
      && requestDetailRows.value.length >= requestDetailLimit.value
      && requestDetailLimit.value < requestDetailDisplayMax,
  )
  const isRequestDetailLogView = computed(() => selectedLogView.value === 'proxy' || selectedLogView.value === 'blocked')
  const showLogLevelFilter = computed(() => selectedLogView.value !== 'playback')
  const logLevelOptions = computed(() => {
    if (selectedLogView.value === 'blocked') {
      return [
        { value: 'all', label: '全部级别' },
        { value: 'blocked', label: '已拦截' },
        { value: 'ban_change', label: '封禁变更' },
      ]
    }
    if (isRequestDetailLogView.value) {
      return [
        { value: 'all', label: '全部级别' },
        { value: 'success', label: 'SUCCESS - 成功' },
        { value: 'redirect', label: 'REDIRECT - 直链/跳转' },
        { value: 'cache', label: 'CACHE - 缓存命中' },
        { value: 'warn', label: 'WARNING - 警告' },
        { value: 'error', label: 'ERROR - 错误' },
        { value: 'blocked', label: 'BLOCKED - 已拦截' },
      ]
    }
    if (selectedLogView.value === 'general') {
      return [
        { value: 'all', label: '全部级别' },
        { value: 'info', label: 'INFO - 信息' },
        { value: 'warn', label: 'WARNING - 警告' },
        { value: 'error', label: 'ERROR - 错误' },
      ]
    }
    return [
      { value: 'all', label: '全部级别' },
      { value: 'success', label: 'SUCCESS - 成功' },
      { value: 'info', label: 'INFO - 信息' },
      { value: 'warn', label: 'WARNING - 警告' },
      { value: 'error', label: 'ERROR - 错误' },
    ]
  })
  const filteredRequestDetailRows = computed(() =>
    requestDetailRows.value.filter((row) => {
      if (selectedLogLevel.value === 'all') return true
      return requestLogLevelMatches(row, selectedLogLevel.value)
    }),
  )
  const visibleLogCount = computed(() => {
    if (isRequestDetailLogView.value) return filteredRequestDetailRows.value.length
    return visibleActivityLogRows.value.length
  })
  const canLoadMoreSelectedLogs = computed(() => {
    if (isRequestDetailLogView.value) return canLoadMoreRequestDetails.value
    if (selectedLogView.value === 'general') return canLoadMoreGeneralLogs.value
    return canLoadMorePlaybackLogs.value
  })
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
        return record.user_agent.toLowerCase().includes(keyword)
      })
      .sort((left, right) => Number(right.created_at) - Number(left.created_at)),
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
    active_windows: rateLimitWindows.value.filter((row) => row.current_count > 0).length,
    blocked_windows: rateLimitWindows.value.filter((row) => row.blocked).length,
    highest_count: rateLimitWindows.value.reduce((max, row) => Math.max(max, row.current_count), 0),
  }))
  const proxyStatusById = computed(() =>
    Object.fromEntries(proxyStatuses.value.map((status) => [status.server_id, status])),
  )
  const operationalServerRows = computed(() =>
    settings.servers.map((server) => ({
      server,
      proxy: proxyStatuses.value.find((status) => status.server_id === server.id),
      connectivity: connectivityStatuses.value.find((status) => status.server_id === server.id),
    })),
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

  function clearNotice() {
    if (noticeTimer !== undefined) {
      window.clearTimeout(noticeTimer)
      noticeTimer = undefined
    }
    notice.value = ''
  }

  function showNotice(message: string) {
    clearNotice()
    notice.value = message
    noticeTimer = window.setTimeout(clearNotice, 3500)
  }

  async function copyText(value: string) {
    const text = value.trim()
    if (!text) {
      return
    }
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text)
      } else {
        const textarea = document.createElement('textarea')
        textarea.value = text
        textarea.style.position = 'fixed'
        textarea.style.opacity = '0'
        document.body.appendChild(textarea)
        textarea.select()
        document.execCommand('copy')
        document.body.removeChild(textarea)
      }
      showNotice(t('链接已复制'))
    } catch {
      showNotice(t('复制失败，请手动复制'))
    }
  }

  function setPage(nextPage: Page) {
    clearNotice()
    closeMobileNav()
    if (page.value === nextPage) {
      if (nextPage === 'logs') void refreshActivityLogs()
      return
    }
    void router.push({ name: nextPage }).catch(() => {
      showNotice(t('页面加载失败，请刷新后重试'))
    })
  }

  function resetLogLimits() {
    playbackLogLimit.value = playbackLogInitialLimit
    generalLogLimit.value = generalLogInitialLimit
    requestDetailLimit.value = requestDetailInitialLimit
  }

  function storedTheme() {
    return readStorage(localStorage, themeKey) || 'light'
  }

  function storedLocale(): Locale {
    return readStorage(localStorage, localeKey) === 'en-US' ? 'en-US' : 'zh-CN'
  }

  function t(source: string) {
    return locale.value === 'en-US' ? translations[source] || source : source
  }

  function localizeValidationText(source: string) {
    if (locale.value !== 'en-US') return source
    const translated = translations[source]
    if (translated) return translated
    const port = source.match(/^端口\s+(.+)$/)
    return port ? `${translations['端口']} ${port[1]}` : source
  }

  function setLocale(nextLocale: Locale) {
    locale.value = nextLocale
    writeStorage(localStorage, localeKey, nextLocale)
    document.documentElement.lang = nextLocale
  }

  function closeMobileNav() {
    const wasOpen = mobileNavOpen.value
    mobileNavOpen.value = false
    if (wasOpen && window.matchMedia('(max-width: 980px)').matches) {
      void nextTick(() => mobileMenuButton.value?.focus())
    }
  }

  function toggleMobileNav() {
    if (mobileNavOpen.value) {
      closeMobileNav()
      return
    }
    mobileNavOpen.value = true
    void nextTick(() => mobileNavCloseButton.value?.focus())
  }

  function trapMobileNavFocus(event: KeyboardEvent) {
    if (event.key !== 'Tab' || !mobileNavOpen.value || !window.matchMedia('(max-width: 980px)').matches) return
    const focusable = [...(mobileSidebar.value?.querySelectorAll<HTMLElement>('button:not(:disabled), a[href]') || [])]
      .filter((element) => element.offsetParent !== null)
    const first = focusable[0]
    const last = focusable.at(-1)
    if (!first || !last) return
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault()
      last.focus()
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault()
      first.focus()
    }
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
      await warmPublicKey()
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
    if (saving.value) return
    saving.value = true
    error.value = ''
    try {
      const response = await api<{ token: string }>(path, {
        method: 'POST',
        body: JSON.stringify(await encryptPayload('credentials', { ...credentials })),
      })
      token.value = response.token
      storeToken(token.value)
      credentials.password = ''
      await loadAppData()
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err)
    } finally {
      saving.value = false
    }
  }

  function resetResourceCache() {
    dataSession += 1
    clearApiKeyRevealState()
    persistedServerIds = new Set()
    passwordChangeToken = ''
    deferredAuthExpiredToken = ''
    settingsLoaded = false
    clientControlLoaded = false
    logConfigLoaded = false
    settingsRequest = undefined
    clientControlRequest = undefined
    logConfigRequest = undefined
    logRequestId += 1
    auditRequestId += 1
  }

  async function loadAppData() {
    error.value = ''
    resetResourceCache()
    const profileResponse = await api<Profile>('/api/profile')
    Object.assign(profile, profileResponse)
    credentials.username = profileResponse.username
    profileForm.username = ''
    mode.value = 'app'
    void refreshUpdateCheck(false)
    await activatePage(page.value)
  }

  async function ensureSettings() {
    if (settingsLoaded) return
    if (settingsRequest) return settingsRequest
    const requestSession = dataSession
    const request = (async () => {
      const response = await api<Settings>('/api/settings')
      if (requestSession !== dataSession || !token.value) return
      Object.assign(settings, response)
      normalizeSettingsServers()
      settingsLoaded = true
    })()
    settingsRequest = request
    try {
      await request
    } finally {
      if (settingsRequest === request) settingsRequest = undefined
    }
  }

  async function ensureClientControl() {
    if (clientControlLoaded) return
    if (clientControlRequest) return clientControlRequest
    const request = (async () => {
      await refreshClientControl()
      if (!clientControlLoaded && token.value) {
        throw new Error(clientControlError.value || t('页面加载失败，请刷新后重试'))
      }
    })()
    clientControlRequest = request
    try {
      await request
    } finally {
      if (clientControlRequest === request) clientControlRequest = undefined
    }
  }

  async function ensureLogConfig() {
    if (logConfigLoaded) return
    if (logConfigRequest) return logConfigRequest
    const request = (async () => {
      await refreshLogConfig()
      if (!logConfigLoaded && token.value) {
        throw new Error(logsError.value || t('页面加载失败，请刷新后重试'))
      }
    })()
    logConfigRequest = request
    try {
      await request
    } finally {
      if (logConfigRequest === request) logConfigRequest = undefined
    }
  }

  async function loadPageData(nextPage: Page) {
    switch (nextPage) {
      case 'home':
        await ensureSettings()
        await refreshDashboard()
        break
      case 'server':
        await ensureSettings()
        await refreshProxyStatuses()
        break
      case 'clients':
        await Promise.all([ensureSettings(), ensureClientControl()])
        await refreshRateLimitStatus()
        break
      case 'users':
        break
      case 'notifications':
        await ensureClientControl()
        break
      case 'logs':
        await ensureSettings()
        await Promise.all([ensureLogConfig(), refreshActivityLogs()])
        break
      case 'account':
        await refreshAuditLogs()
        break
      case 'backup':
        break
    }
  }

  async function activatePage(nextPage: Page) {
    if (!token.value || mode.value !== 'app') return
    const activation = ++pageActivation
    error.value = ''
    pageLoading.value = true
    pageReady.value = false
    stopPagePolling()
    let loaded = false
    try {
      await loadPageData(nextPage)
      loaded = true
    } catch (err) {
      if (activation === pageActivation) {
        error.value = err instanceof Error ? err.message : String(err)
      }
    } finally {
      if (activation === pageActivation) {
        pageLoading.value = false
        pageReady.value = loaded
        if (loaded && page.value === nextPage && mode.value === 'app') {
          startPagePolling(nextPage)
        }
      }
    }
  }

  function retryPage() {
    error.value = ''
    void activatePage(page.value)
  }

  async function refreshOperationalData() {
    try {
      const [health, statuses, stats, rateLimit, connectivity] = await Promise.all([
        api<DetailedHealth>('/api/monitoring/healthz'),
        api<ProxyStatus[]>('/api/monitoring/proxy-status'),
        api<RequestStatsDaily[]>('/api/monitoring/stats'),
        api<PlaybackRateWindowStatus[]>('/api/client-control/rate-limit/status'),
        api<ConnectivityCheckStatus[]>('/api/monitoring/connectivity'),
      ])
      detailedHealth.value = health
      proxyStatuses.value = statuses
      requestStats.value = stats
      rateLimitWindows.value = rateLimit
      connectivityStatuses.value = connectivity
    } catch (err) {
      healthError.value = err instanceof Error ? err.message : String(err)
    }
  }

  async function refreshProxyStatuses() {
    try {
      proxyStatuses.value = await api<ProxyStatus[]>('/api/monitoring/proxy-status')
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err)
    }
  }

  async function refreshUpdateCheck(force = true) {
    updateChecking.value = true
    updateCheckError.value = ''
    try {
      updateCheck.value = await api<UpdateCheck>(`/api/app-info/update-check${force ? '?force=true' : ''}`)
    } catch (err) {
      updateCheck.value = null
      updateCheckError.value = err instanceof Error ? err.message : String(err)
    } finally {
      updateChecking.value = false
    }
  }

  async function refreshLogConfig() {
    const requestSession = dataSession
    logsError.value = ''
    try {
      const response = await api<SystemLogConfig>('/api/settings/log-config')
      if (requestSession !== dataSession || !token.value) return
      Object.assign(logConfig, response)
      logConfigLoaded = true
    } catch (err) {
      logsError.value = err instanceof Error ? err.message : String(err)
    }
  }

  async function refreshClientControl() {
    const requestSession = dataSession
    clientControlError.value = ''
    try {
      const response = await api<ClientControlConfig>('/api/client-control')
      if (requestSession !== dataSession || !token.value) return
      applyClientControlConfig(response)
    } catch (err) {
      clientControlError.value = err instanceof Error ? err.message : String(err)
    }
  }

  async function refreshClientControlLiveData() {
    const requestSession = dataSession
    try {
      const response = await api<ClientControlConfig>('/api/client-control')
      if (requestSession !== dataSession || !token.value) return
      applyClientControlLiveData(response)
    } catch {
      // Live refresh should not interrupt editing client control settings.
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
    const requestId = ++auditRequestId
    auditLogsError.value = ''
    try {
      const params = new URLSearchParams({ limit: '120' })
      if (selectedAuditAction.value !== 'all') params.set('action', selectedAuditAction.value)
      if (auditKeywordFilter.value.trim()) params.set('keyword', auditKeywordFilter.value.trim())
      const rows = await api<AuditLogEntry[]>(`/api/monitoring/audit-logs?${params.toString()}`)
      if (requestId !== auditRequestId) return
      auditLogs.value = rows
    } catch (err) {
      if (requestId !== auditRequestId) return
      auditLogs.value = []
      auditLogsError.value = err instanceof Error ? err.message : String(err)
    }
  }

  async function saveSettings() {
    if (!settingsLoaded) {
      error.value = t('页面加载失败，请刷新后重试')
      return
    }
    if (saving.value) return
    saving.value = true
    clearNotice()
    error.value = ''
    try {
      const payload = buildSettingsPayload()
      const response = await api<Settings>('/api/settings', {
        method: 'PUT',
        body: JSON.stringify(await encryptPayload('settings', payload)),
      })
      Object.assign(settings, response)
      normalizeSettingsServers()
      settingsLoaded = true
      showNotice(t('服务器配置已保存，反代监听器已差量同步'))
      await refreshProxyStatuses()
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err)
    } finally {
      saving.value = false
    }
  }

  async function validateSettings() {
    if (!settingsLoaded) {
      error.value = t('页面加载失败，请刷新后重试')
      return
    }
    if (saving.value) return
    saving.value = true
    error.value = ''
    clearNotice()
    validationResults.value = []
    try {
      const payload = buildSettingsPayload()
      const response = await api<ValidationResponse>('/api/settings/validate', {
        method: 'POST',
        body: JSON.stringify(await encryptPayload('settings', payload)),
      })
      validationResults.value = response.results
      showNotice(response.ok ? t('配置测试通过') : t('配置测试完成，请查看警告项'))
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err)
    } finally {
      saving.value = false
    }
  }

  async function saveLogConfig() {
    if (!logConfigLoaded) {
      logsError.value = t('页面加载失败，请刷新后重试')
      return
    }
    logsError.value = ''
    clearNotice()
    try {
      Object.assign(logConfig, await api<SystemLogConfig>('/api/settings/log-config', {
        method: 'PUT',
        body: JSON.stringify(await encryptPayload('log_config', { ...logConfig })),
      }))
      logConfigLoaded = true
      showNotice(t('日志配置已保存'))
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

  function refreshLogsWithReset() {
    resetLogLimits()
    void refreshActivityLogs()
  }

  function handleLogViewChange() {
    selectedLogLevel.value = 'all'
    selectedRequestPathType.value = 'all'
    refreshLogsWithReset()
  }

  async function restartProxyServer(server: EmbyServerConfig) {
    restartingServerId.value = server.id
    clearNotice()
    error.value = ''
    try {
      const response = await api<Settings>('/api/settings/restart-proxy', {
        method: 'POST',
        body: JSON.stringify({ server_id: server.id }),
      })
      Object.assign(settings, response)
      normalizeSettingsServers()
      settingsLoaded = true
      await refreshProxyStatuses()
      showNotice(`${server.name || t('服务器')} ${t('反代服务已重启')}`)
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err)
    } finally {
      restartingServerId.value = ''
    }
  }

  async function toggleProxyServer(server: EmbyServerConfig) {
    const nextEnabled = !server.enabled
    restartingServerId.value = server.id
    clearNotice()
    error.value = ''
    try {
      const response = await api<Settings>('/api/settings/toggle-proxy', {
        method: 'POST',
        body: JSON.stringify({ server_id: server.id, enabled: nextEnabled }),
      })
      Object.assign(settings, response)
      normalizeSettingsServers()
      settingsLoaded = true
      showNotice(`${server.name || t('服务器')} ${nextEnabled ? t('已开启') : t('已关闭')}`)
      await refreshProxyStatuses()
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err)
    } finally {
      restartingServerId.value = ''
    }
  }

  function buildSettingsPayload() {
    const servers = settings.servers
      .map((server) => ({
        ...server,
        name: server.name.trim(),
        emby_host: server.emby_host.trim(),
        emby_api_key: server.emby_api_key.trim(),
        port: Number(server.port),
        block_web_ui: Boolean(server.block_web_ui),
        real_ip_mode: server.real_ip_mode || 'auto',
        real_ip_header: server.real_ip_header.trim(),
        trusted_proxy_cidrs: server.trusted_proxy_cidrs.trim(),
      }))
    const incompleteServer = servers.find(
      (server) =>
        !server.emby_host ||
        (!server.emby_api_key && !persistedServerIds.has(server.id)),
    )
    if (incompleteServer) {
      throw new Error(
        `${incompleteServer.name || t('服务器')}：${t('请填写 Emby 地址和 API Key')}`,
      )
    }
    const primary = servers.find((server) => server.enabled) ?? servers[0]
    return {
      ...settings,
      servers,
      emby_host: primary?.emby_host ?? '',
      emby_api_key: primary?.emby_api_key ?? '',
      port: primary?.port ?? 8096,
      openlist_addr: emptyToNull(settings.openlist_addr),
      openlist_token: emptyToNull(settings.openlist_token),
    }
  }

  function normalizeSettingsServers() {
    settings.servers = settings.servers.map((server) => ({
      ...server,
      block_web_ui: Boolean(server.block_web_ui),
      real_ip_mode: server.real_ip_mode || 'auto',
      real_ip_header: server.real_ip_header || '',
      trusted_proxy_cidrs: server.trusted_proxy_cidrs || '',
    }))
    persistedServerIds = new Set(settings.servers.map((server) => server.id))
  }

  function addServer() {
    const lastPort = settings.servers.at(-1)?.port ?? 8096
    settings.servers.push(newBlankServer(settings.servers.length + 1, lastPort + 1))
  }

  function newBlankServer(index: number, port: number): EmbyServerConfig {
    return {
      id: newServerId(),
      name: `${t('服务器')} ${index}`,
      emby_host: '',
      emby_api_key: '',
      port,
      enabled: true,
      block_web_ui: false,
      real_ip_mode: 'auto',
      real_ip_header: '',
      trusted_proxy_cidrs: '',
    }
  }

  function needsRealIpHeader(server: EmbyServerConfig) {
    return server.real_ip_mode === 'header' || server.real_ip_mode === 'header_list'
  }

  function updateRealIpMode(server: EmbyServerConfig) {
    if (server.real_ip_mode === 'header_list' && !server.real_ip_header.trim()) {
      server.real_ip_header = defaultCdnHeaders
    }
  }

  function isServerExpanded(serverId: string) {
    return Boolean(expandedServerCards.value[serverId])
  }

  function toggleServerExpanded(serverId: string) {
    expandedServerCards.value = {
      ...expandedServerCards.value,
      [serverId]: !expandedServerCards.value[serverId],
    }
  }

  function removeServer(serverId: string) {
    settings.servers = settings.servers.filter((server) => server.id !== serverId)
    persistedServerIds.delete(serverId)
    clearApiKeyRevealState()
    const { [serverId]: _collapsed, ...expandedServers } = expandedServerCards.value
    expandedServerCards.value = expandedServers
  }

  function newServerId() {
    return randomId('server')
  }

  function isApiKeyVisible(serverId: string) {
    return Boolean(visibleApiKeyServers.value[serverId])
  }

  function isApiKeyRevealLoading(serverId: string) {
    return Boolean(revealingApiKeyServers.value[serverId])
  }

  function apiKeyInputValue(server: EmbyServerConfig) {
    if (server.emby_api_key) return server.emby_api_key
    if (!isApiKeyVisible(server.id)) return ''
    return revealedApiKeys.value[server.id] || ''
  }

  function updateApiKeyInput(server: EmbyServerConfig, event: Event) {
    const target = event.target
    if (!(target instanceof HTMLInputElement)) return
    server.emby_api_key = target.value
    const { [server.id]: _revealed, ...revealedKeys } = revealedApiKeys.value
    revealedApiKeys.value = revealedKeys
  }

  async function toggleApiKeyVisible(server: EmbyServerConfig) {
    const serverId = server.id
    if (isApiKeyVisible(serverId)) {
      const { [serverId]: _visible, ...visibleServers } = visibleApiKeyServers.value
      visibleApiKeyServers.value = visibleServers
      const { [serverId]: _revealed, ...revealedKeys } = revealedApiKeys.value
      revealedApiKeys.value = revealedKeys
      return
    }
    if (server.emby_api_key) {
      visibleApiKeyServers.value = {
        ...visibleApiKeyServers.value,
        [serverId]: true,
      }
      return
    }
    if (!persistedServerIds.has(serverId)) {
      visibleApiKeyServers.value = {
        ...visibleApiKeyServers.value,
        [serverId]: true,
      }
      return
    }
    if (isApiKeyRevealLoading(serverId)) return
    revealingApiKeyServers.value = {
      ...revealingApiKeyServers.value,
      [serverId]: true,
    }
    const revealGeneration = apiKeyRevealGeneration
    error.value = ''
    try {
      const response = await api<{ api_key: string }>(
        `/api/settings/servers/${encodeURIComponent(serverId)}/api-key`,
        { method: 'POST' },
      )
      if (revealGeneration !== apiKeyRevealGeneration) return
      revealedApiKeys.value = {
        ...revealedApiKeys.value,
        [serverId]: response.api_key,
      }
      visibleApiKeyServers.value = {
        ...visibleApiKeyServers.value,
        [serverId]: true,
      }
    } catch (err) {
      if (revealGeneration === apiKeyRevealGeneration) {
        error.value = err instanceof Error ? err.message : String(err)
      }
    } finally {
      if (revealGeneration === apiKeyRevealGeneration) {
        const { [serverId]: _revealing, ...revealingServers } = revealingApiKeyServers.value
        revealingApiKeyServers.value = revealingServers
      }
    }
  }

  function apiKeyPlaceholder(server: EmbyServerConfig) {
    return persistedServerIds.has(server.id) && !server.emby_api_key ? '************' : ''
  }

  function clearApiKeyRevealState() {
    apiKeyRevealGeneration += 1
    visibleApiKeyServers.value = {}
    revealedApiKeys.value = {}
    revealingApiKeyServers.value = {}
  }

  async function saveAccount() {
    if (savingAccount.value) return
    clearNotice()
    error.value = ''

    const username = profileForm.username.trim()
    const updateUsername = Boolean(username && username !== profile.username)
    const updatePassword = Boolean(
      passwordForm.current_password
      || passwordForm.new_password
      || passwordForm.confirm_password,
    )

    if (updatePassword && !passwordForm.current_password) {
      error.value = t('当前密码不能为空')
      return
    }
    if (updatePassword && !passwordForm.new_password) {
      error.value = t('新密码不能为空')
      return
    }
    if (updatePassword && !passwordForm.confirm_password) {
      error.value = t('确认新密码不能为空')
      return
    }
    if (updatePassword && passwordForm.new_password !== passwordForm.confirm_password) {
      error.value = t('两次输入的新密码不一致')
      return
    }
    if (!updateUsername && !updatePassword) {
      profileForm.username = ''
      showNotice(t('未检测到需要保存的修改'))
      return
    }

    const operation = ++accountSaveOperation
    let operationToken = token.value
    const operationIsCurrent = () => (
      operation === accountSaveOperation
      && token.value === operationToken
      && mode.value === 'app'
    )
    savingAccount.value = true
    let passwordUpdated = false
    try {
      if (updatePassword) {
        const encryptedPassword = await encryptPayload('password', {
          current_password: passwordForm.current_password,
          new_password: passwordForm.new_password,
        })
        if (!operationIsCurrent()) return
        passwordChangeToken = operationToken
        let response: { changed: boolean; token: string }
        try {
          response = await api<{ changed: boolean; token: string }>('/api/change-password', {
            method: 'POST',
            body: JSON.stringify(encryptedPassword),
          })
        } catch (err) {
          const expiredToken = deferredAuthExpiredToken
          passwordChangeToken = ''
          deferredAuthExpiredToken = ''
          if (expiredToken) handleAuthExpired(expiredToken)
          throw err
        }
        passwordChangeToken = ''
        deferredAuthExpiredToken = ''
        if (!operationIsCurrent()) return
        token.value = response.token
        operationToken = response.token
        storeToken(response.token)
        passwordForm.current_password = ''
        passwordForm.new_password = ''
        passwordForm.confirm_password = ''
        passwordUpdated = true
      }

      if (updateUsername) {
        const encryptedProfile = await encryptPayload('profile', { username })
        if (!operationIsCurrent()) return
        const response = await api<Profile>('/api/profile', {
          method: 'PUT',
          body: JSON.stringify(encryptedProfile),
        })
        if (!operationIsCurrent()) return
        Object.assign(profile, response)
        credentials.username = response.username
        profileForm.username = ''
      }

      const successMessage = updateUsername && updatePassword
        ? '账户设置已更新'
        : updatePassword
          ? '管理员密码已更新'
          : '账户资料已更新'
      showNotice(t(successMessage))
    } catch (err) {
      if (!operationIsCurrent()) return
      const message = err instanceof Error ? err.message : String(err)
      error.value = message === 'current password is incorrect'
        ? t('当前密码不正确')
        : message
      if (passwordUpdated) {
        showNotice(t('密码已更新，但用户名修改失败'))
      }
    } finally {
      if (operation === accountSaveOperation) savingAccount.value = false
    }
  }

  async function saveClientControl() {
    if (!clientControlLoaded) {
      clientControlError.value = t('页面加载失败，请刷新后重试')
      return
    }
    if (savingClientControl.value) return
    savingClientControl.value = true
    clearNotice()
    clientControlError.value = ''
    try {
      const response = await api<ClientControlConfig>('/api/client-control', {
        method: 'PUT',
        body: JSON.stringify(await encryptPayload('client_control', sanitizeClientControl())),
      })
      applyClientControlConfig(response)
      showNotice(t('客户端管控规则已保存'))
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
      concurrent_playback_limit_enabled: clientControl.concurrent_playback_limit_enabled,
      concurrent_playback_limit_max: Number(clientControl.concurrent_playback_limit_max),
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
    clearNotice()
    const url = webhook.url.trim()
    if (!url) {
      clientControlError.value = t('Webhook URL 不能为空')
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
            title: t('EmbyPanel 通知测试'),
            text: t('Webhook POST 测试成功'),
          }),
        ),
      })
      showNotice(t('Webhook 测试发送成功'))
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
    clientControlLoaded = true
  }

  function applyClientControlLiveData(response: ClientControlConfig) {
    clientControl.records = response.records || []
    clientControl.rate_limit_blocks = response.rate_limit_blocks || []
  }

  function newWebhookConfig(): WebhookNotifyConfig {
    return {
      id: newWebhookId(),
      enabled: true,
      name: t('新建 Webhook'),
      url: '',
      secret: '',
    }
  }

  function normalizeWebhook(webhook: Partial<WebhookNotifyConfig>): WebhookNotifyConfig {
    return {
      id: webhook.id?.trim() || newWebhookId(),
      enabled: Boolean(webhook.enabled),
      name: webhook.name?.trim() || t('新建 Webhook'),
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
    return randomId('webhook')
  }

  async function addClientRule() {
    clientControlError.value = ''
    clearNotice()
    const userAgent = manualClientRule.user_agent.trim()
    if (!userAgent) {
      clientControlError.value = t('UA 关键字不能为空')
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
      showNotice(t('UA 拦截规则已添加'))
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
      await refreshRateLimitStatus()
      showNotice(playbackRateUnblockNotice(record.action))
    } catch (err) {
      clientControlError.value = err instanceof Error ? err.message : String(err)
    }
  }

  async function unblockRateLimitWindow(row: PlaybackRateWindowStatus) {
    if (!row.block_id) return
    clientControlError.value = ''
    try {
      const response = await api<ClientControlConfig>('/api/client-control/rate-blocks/unblock', {
        method: 'POST',
        body: JSON.stringify(await encryptPayload('rate_limit_block', { id: row.block_id })),
      })
      applyClientControlConfig(response)
      await refreshRateLimitStatus()
      showNotice(playbackRateUnblockNotice(row.block_action))
    } catch (err) {
      clientControlError.value = err instanceof Error ? err.message : String(err)
    }
  }

  async function deleteClientRule(record: ClientRuleRecord) {
    clientControlError.value = ''
    const keyword = clientKeyword(record)
    const confirmed = await confirmAction({
      title: t('删除 UA 规则'),
      description: locale.value === 'en-US'
        ? `Delete the UA rule "${keyword}"?`
        : `确定删除 UA 规则「${keyword}」吗？`,
      confirmText: t('确认删除'),
      cancelText: t('取消'),
      tone: 'danger',
    })
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
      showNotice(t('UA 规则已删除'))
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

  function rateLimitBlockReason(record: PlaybackRateBlockRecord) {
    const note = record.note?.trim()
    if (note.includes('同时播放')) return t('同时播放超限')
    if (note.includes('频率超限') || note.includes('窗口') || note.includes('阈值')) return t('播放频率限制')
    return playbackRateActionLabel(record.action)
  }

  function playbackRateActionLabel(action?: string) {
    if (action === 'block_user') return t('封禁用户')
    if (action === 'disable_user') return t('禁用用户')
    if (action === 'mixed') return t('混合模式')
    if (action === 'block_ip') return t('屏蔽 IP')
    return '--'
  }

  function playbackRateUnblockNotice(action?: string) {
    if (action === 'block_user') return t('用户封禁已解除')
    if (action === 'disable_user') return t('用户封禁已解除')
    if (action === 'mixed') return t('混合封禁已解除')
    return t('IP 屏蔽已解除')
  }

  function formatIpLocation(location?: IpLocation) {
    if (!location) return ''
    return [
      location.country_name,
      location.region_name,
      location.city_name,
      location.district_name,
      location.isp_domain,
    ]
      .map((value) => value?.trim())
      .filter((value, index, values) => value && values.indexOf(value) === index)
      .join(' ')
  }

  function ipWithLocation(ip: string | null | undefined, location?: IpLocation) {
    const ipText = ip?.trim() || '--'
    const locationText = formatIpLocation(location)
    return locationText ? `${ipText} · ${locationText}` : ipText
  }

  async function logout() {
    if (logoutInProgress) return
    logoutInProgress = true
    accountSaveOperation += 1
    savingAccount.value = false
    const logoutToken = token.value
    error.value = ''
    try {
      if (logoutToken) {
        const headers = new Headers({
          Authorization: `Bearer ${logoutToken}`,
          'Content-Type': 'application/json',
        })
        const response = await fetch('/api/logout', { method: 'POST', headers })
        if (!response.ok && response.status !== 401) {
          throw new Error(await response.text())
        }
        if (token.value !== logoutToken) return
      }
      token.value = ''
      clearPasswordFields()
      void router.replace({ name: 'home' }).catch(() => undefined)
      mobileNavOpen.value = false
      clearStoredToken()
      removeStorage(localStorage, pageKey)
      pageActivation += 1
      pageLoading.value = false
      pageReady.value = false
      stopPagePolling()
      resetResourceCache()
      mode.value = 'login'
    } catch {
      if (token.value === logoutToken) {
        error.value = t('退出登录失败，请重试')
      }
    } finally {
      logoutInProgress = false
    }
  }

  function storedToken() {
    const sessionToken = readStorage(sessionStorage, tokenKey)
    if (sessionToken) return sessionToken
    const legacyToken = readStorage(localStorage, tokenKey)
    if (legacyToken) {
      writeStorage(sessionStorage, tokenKey, legacyToken)
      removeStorage(localStorage, tokenKey)
    }
    return legacyToken
  }

  function storeToken(value: string) {
    writeStorage(sessionStorage, tokenKey, value)
    removeStorage(localStorage, tokenKey)
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
    } catch (err) {
      playbackSessions.value = []
      playbackError.value = err instanceof Error ? err.message : String(err)
    } finally {
      playbackLoading.value = false
    }
  }

  async function fetchPlaybackArtwork(serverId: string, itemId: string, signal: AbortSignal) {
    const requestToken = token.value
    if (!requestToken || signal.aborted) return null
    const params = new URLSearchParams({ server_id: serverId, item_id: itemId })
    const headers = new Headers({ Authorization: `Bearer ${requestToken}` })
    const response = await fetch(`/api/monitoring/playback-image?${params.toString()}`, {
      headers,
      signal,
    })
    if (response.status === 404) return null
    if (!response.ok) {
      const message = await response.text()
      if (response.status === 401 && token.value === requestToken) {
        handleAuthExpired(requestToken)
      }
      throw new Error(message)
    }
    if (token.value !== requestToken || signal.aborted) return null
    const blob = await response.blob()
    return token.value === requestToken && !signal.aborted && blob.type.startsWith('image/') ? blob : null
  }

  async function refreshActivityLogs() {
    if (!token.value) return
    const requestId = ++logRequestId
    logsLoading.value = true
    logsError.value = ''
    try {
      if (selectedLogView.value === 'playback') {
        const rows = await fetchActivityLogs('playback', playbackLogLimit.value)
        if (requestId === logRequestId) {
          activityLogs.value = rows
          proxyRequestDetails.value = []
        }
      } else if (selectedLogView.value === 'general') {
        const rows = await fetchActivityLogs('general', generalLogLimit.value)
        if (requestId === logRequestId) {
          activityLogs.value = rows
          proxyRequestDetails.value = []
        }
      } else {
        const rows = await fetchProxyRequestDetails()
        if (requestId === logRequestId) {
          proxyRequestDetails.value = rows
          activityLogs.value = []
        }
      }
    } catch (err) {
      if (requestId === logRequestId) {
        activityLogs.value = []
        proxyRequestDetails.value = []
        logsError.value = err instanceof Error ? err.message : String(err)
      }
    } finally {
      if (requestId === logRequestId) logsLoading.value = false
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
    if (selectedLogView.value === 'playback') params.set('kind', 'playback')
    if (selectedLogView.value === 'general') params.set('kind', 'general')
    if (showLogLevelFilter.value && selectedLogLevel.value !== 'all') params.set('level', selectedLogLevel.value)
    if (logKeywordFilter.value.trim()) params.set('keyword', logKeywordFilter.value.trim())
    if (logSince.value) params.set('since_ms', String(new Date(logSince.value).getTime()))
    if (logUntil.value) params.set('until_ms', String(new Date(logUntil.value).getTime()))
    return params
  }

  async function fetchProxyRequestDetails() {
    const params = new URLSearchParams({ limit: String(requestDetailLimit.value) })
    if (selectedLogServer.value !== 'all') params.set('server_id', selectedLogServer.value)
    if (selectedRequestPathType.value !== 'all') params.set('path_type', selectedRequestPathType.value)
    if (selectedLogView.value === 'blocked' && selectedLogLevel.value !== 'all') params.set('level', selectedLogLevel.value)
    if (logKeywordFilter.value.trim()) params.set('keyword', logKeywordFilter.value.trim())
    if (logSince.value) params.set('since_ms', String(new Date(logSince.value).getTime()))
    if (logUntil.value) params.set('until_ms', String(new Date(logUntil.value).getTime()))
    const endpoint = selectedLogView.value === 'blocked'
      ? '/api/monitoring/block-logs'
      : '/api/monitoring/request-details'
    return api<ProxyRequestDetail[]>(`${endpoint}?${params.toString()}`)
  }

  function loadMorePlaybackLogs() {
    if (!canLoadMorePlaybackLogs.value || logsLoading.value) return
    playbackLogLimit.value = Math.min(playbackLogLimit.value + activityLogLoadStep, activityLogMaxLimit)
    void refreshActivityLogs()
  }

  function loadMoreGeneralLogs() {
    if (!canLoadMoreGeneralLogs.value || logsLoading.value) return
    generalLogLimit.value = Math.min(generalLogLimit.value + activityLogLoadStep, activityLogMaxLimit)
    void refreshActivityLogs()
  }

  function loadMoreRequestDetails() {
    if (!canLoadMoreRequestDetails.value || logsLoading.value) return
    requestDetailLimit.value = Math.min(requestDetailLimit.value + requestDetailLoadStep, requestDetailDisplayMax)
    void refreshActivityLogs()
  }

  function loadMoreSelectedLogs() {
    if (isRequestDetailLogView.value) return loadMoreRequestDetails()
    if (selectedLogView.value === 'general') return loadMoreGeneralLogs()
    return loadMorePlaybackLogs()
  }

  function handleScrollableLogListScroll(event: Event, loadMore: () => void) {
    const element = event.currentTarget as HTMLElement | null
    if (!element) return
    const distanceToBottom = element.scrollHeight - element.scrollTop - element.clientHeight
    if (distanceToBottom <= 80) loadMore()
  }

  function requestSeverity(row: ProxyRequestDetail) {
    if (row.event_type === 'block') return 'blocked'
    if (row.event_type === 'unblock') return 'success'
    if (row.blocked) return 'blocked'
    if (row.cache_hit) return 'cache'
    if (row.status_code >= 500) return 'error'
    if (row.status_code >= 400) return 'warn'
    if (row.status_code >= 300) return 'redirect'
    return 'success'
  }

  function requestLogLevelMatches(row: ProxyRequestDetail, level: string) {
    if (level === 'ban_change') return row.event_type === 'block' || row.event_type === 'unblock'
    return requestSeverity(row) === level
  }

  function requestSeverityLabel(row: ProxyRequestDetail) {
    if (row.event_type === 'block') return t('封禁动作')
    if (row.event_type === 'unblock') return t('解除封禁动作')
    const severity = requestSeverity(row)
    if (severity === 'blocked') return t('已拦截')
    if (severity === 'cache') return t('缓存')
    if (severity === 'redirect') return t('跳转')
    if (severity === 'warn') return t('警告')
    if (severity === 'error') return t('错误')
    return t('成功')
  }

  function startPagePolling(activePage: Page) {
    stopPagePolling()
    const interval = activePage === 'logs'
      ? 3000
      : ['home', 'server', 'clients'].includes(activePage)
        ? 10000
        : 0
    if (!interval) return
    dashboardTimer = window.setInterval(() => void pollPage(activePage), interval)
  }

  async function pollPage(activePage: Page) {
    if (pagePollRunning || document.hidden || page.value !== activePage || mode.value !== 'app') return
    pagePollRunning = true
    try {
      if (activePage === 'home') {
        await ensureSettings()
        await refreshDashboard()
      } else if (activePage === 'server') {
        await ensureSettings()
        await refreshProxyStatuses()
      } else if (activePage === 'clients') {
        await Promise.all([
          ensureSettings(),
          clientControlLoaded ? refreshClientControlLiveData() : ensureClientControl(),
          refreshRateLimitStatus(),
        ])
      } else if (activePage === 'logs') {
        await Promise.all([ensureLogConfig(), refreshActivityLogs()])
      }
    } catch (err) {
      if (page.value === activePage && mode.value === 'app') {
        error.value = err instanceof Error ? err.message : String(err)
      }
    } finally {
      pagePollRunning = false
    }
  }

  function stopPagePolling() {
    if (dashboardTimer !== undefined) {
      window.clearInterval(dashboardTimer)
      dashboardTimer = undefined
    }
  }

  function handleAuthExpired(expiredToken: string) {
    if (!expiredToken || token.value !== expiredToken) return
    accountSaveOperation += 1
    savingAccount.value = false
    token.value = ''
    clearPasswordFields()
    mobileNavOpen.value = false
    clearStoredToken()
    pageActivation += 1
    pageLoading.value = false
    pageReady.value = false
    stopPagePolling()
    resetResourceCache()
    mode.value = 'login'
    error.value = t('登录已过期，请重新登录')
  }

  function clearPasswordFields() {
    credentials.password = ''
    passwordForm.current_password = ''
    passwordForm.new_password = ''
    passwordForm.confirm_password = ''
  }

  const formatUptime = (seconds: number | undefined) => formatUptimeValue(seconds, locale.value)
  const formatTimestamp = (value: string) => formatTimestampValue(value, locale.value)
  const formatLogTime = (value: number) => formatLogTimeValue(value, locale.value)
  const formatTimestampMs = (value: number | null | undefined) =>
    formatTimestampMsValue(value, locale.value)

  function formatServerName(serverId: string) {
    return settings.servers.find((server) => server.id === serverId)?.name || serverId || '--'
  }

  function proxyStatusLabel(status: ProxyStatus | undefined) {
    if (!status) return t('未启动')
    if (!status.enabled) return t('未启用')
    return status.listening ? t('监听中') : t('未监听')
  }

  function connectivityStatusLabel(status: ConnectivityCheckStatus) {
    if (!status.checked_at_ms) return t('未巡检')
    return status.ok ? t('正常') : t('异常')
  }

  function healthPartLabel(ok: boolean | null) {
    if (ok === null) return t('未配置')
    return ok ? t('正常') : t('异常')
  }

  function failedDuration(status: ConnectivityCheckStatus) {
    if (!status.failed_since_ms) return t('无失败')
    const seconds = Math.max(0, Math.floor((Date.now() - status.failed_since_ms) / 1000))
    if (seconds >= 3600) return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`
    if (seconds >= 60) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`
    return `${seconds}s`
  }

  function proxyStatusClass(status: ProxyStatus | undefined) {
    return status?.listening ? 'allowed' : 'blocked'
  }

  function validationClass(result: ValidationResult) {
    return result.ok ? 'success' : 'warn'
  }

  function logLevelLabel(level: ActivityLogEntry['level']) {
    if (level === 'success') return t('成功')
    if (level === 'error') return t('错误')
    if (level === 'warn') return t('警告')
    return t('信息')
  }

  function requestPathTypeLabel(type: string) {
    if (type === 'rate_limit_action') return t('封禁动作')
    if (type === 'video_stream') return t('视频流')
    if (type === 'playback_info') return t('播放信息')
    if (type === 'system_info') return t('系统信息')
    if (type === 'base_html_player') return t('播放器脚本')
    return t('普通代理')
  }

  function requestRowTitle(row: ProxyRequestDetail) {
    if (row.event_type === 'block') return `${t('封禁')} ${row.playback_ip || '--'}`
    if (row.event_type === 'unblock') return `${t('解除封禁')} ${row.playback_ip || '--'}`
    return `${row.method} ${row.path}`
  }

  function clientKeyword(record: ClientRuleRecord) {
    return record.user_agent || record.client_name || '--'
  }

  watch(locale, (nextLocale) => {
    document.documentElement.lang = nextLocale
    clearNotice()
    error.value = ''
    playbackError.value = ''
    logsError.value = ''
    updateCheckError.value = ''
    overviewError.value = ''
    healthError.value = ''
    clientControlError.value = ''
    backupError.value = ''
  })

  watch(darkMode, (enabled) => {
    document.documentElement.classList.toggle('dark', enabled)
  }, { immediate: true })

  watch(page, (nextPage, previousPage) => {
    if (previousPage === 'server' && nextPage !== 'server') clearApiKeyRevealState()
    clearNotice()
    closeMobileNav()
    writeStorage(localStorage, pageKey, nextPage)
    if (mode.value === 'app') void activatePage(nextPage)
  })

  onMounted(async () => {
    document.documentElement.lang = locale.value
    const storedPageValue = readStorage(localStorage, pageKey)
    const preferredPage = storedPage()
    if (window.location.hash === '#/' && storedPageValue && preferredPage !== 'home') {
      await router.replace({ name: preferredPage }).catch(() => undefined)
    }
    await bootstrap()
  })
  onBeforeUnmount(() => {
    pageActivation += 1
    stopPagePolling()
  })

  return {
    mode,
    page,
    darkMode,
    locale,
    mobileNavOpen,
    mobileMenuButton,
    mobileNavCloseButton,
    mobileSidebar,
    saving,
    pageLoading,
    pageReady,
    error,
    notice,
    credentials,
    logoUrl,
    profile,
    appInfo,
    menu,
    updateCheck,
    updateChecking,
    updateCheckError,
    t,
    showNotice,
    clearNotice,
    confirmAction,
    promptAction,
    setLocale,
    closeMobileNav,
    toggleMobileNav,
    trapMobileNavFocus,
    toggleTheme,
    setupAdmin,
    login,
    refreshUpdateCheck,
    setPage,
    retryPage,
    logout,
    api,
    encryptPayload,
    profileForm,
    savingAccount,
    passwordForm,
    auditLogs,
    auditLogsError,
    auditKeywordFilter,
    selectedAuditAction,
    auditActionOptions,
    saveAccount,
    refreshAuditLogs,
    formatTimestampMs,
    backupError,
    backupBusy,
    backupFileInput,
    exportBackup,
    importBackup,
    handleBackupFileSelected,
    clientControl,
    clientControlError,
    savingClientControl,
    addingClientRule,
    clientStatusFilter,
    clientKeywordFilter,
    manualClientRule,
    rateLimitWindows,
    playbackLimitActionOptions,
    activeRateLimitBlocks,
    allowedClientCount,
    blockedClientCount,
    clientRuleRows,
    refreshClientControl,
    saveClientControl,
    refreshRateLimitStatus,
    unblockRateLimit,
    addClientRule,
    clearClientFilters,
    clientKeyword,
    deleteClientRule,
    toggleClientRule,
    formatIpLocation,
    formatServerName,
    formatTimestamp,
    playbackRateActionLabel,
    rateLimitBlockIp,
    rateLimitBlockReason,
    logsLoading,
    logsError,
    selectedLogServer,
    selectedLogView,
    selectedLogLevel,
    selectedRequestPathType,
    logKeywordFilter,
    logSince,
    logUntil,
    logConfig,
    activityLogMaxLimit,
    requestDetailDisplayMax,
    requestDetailPersistDays,
    requestDetailPersistMax,
    logServers,
    selectedLogViewLabel,
    visibleActivityLogRows,
    isRequestDetailLogView,
    showLogLevelFilter,
    logLevelOptions,
    filteredRequestDetailRows,
    visibleLogCount,
    canLoadMoreSelectedLogs,
    refreshActivityLogs,
    exportLogs,
    handleLogViewChange,
    refreshLogsWithReset,
    handleScrollableLogListScroll,
    loadMoreSelectedLogs,
    formatLogTime,
    logLevelLabel,
    ipWithLocation,
    isHttpUrl,
    copyText,
    requestOutcomeClass,
    requestSeverity,
    requestSeverityLabel,
    requestRowTitle,
    requestPathTypeLabel,
    saveLogConfig,
    testingWebhook,
    addWebhook,
    removeWebhook,
    testWebhook,
    mediaOverviews,
    serverHealth,
    detailedHealth,
    healthError,
    overviewError,
    playbackSessions,
    playbackLoading,
    playbackError,
    mediaOverviewTotals,
    requestStatsTotals,
    operationalServerRows,
    rateLimitOverview,
    activePlayCount,
    formatUptime,
    formatBytes,
    refreshOperationalData,
    proxyStatusClass,
    proxyStatusLabel,
    connectivityStatusLabel,
    healthPartLabel,
    failedDuration,
    unblockRateLimitWindow,
    refreshPlaybackSessions,
    fetchPlaybackArtwork,
    formatTicks,
    settings,
    restartingServerId,
    validationResults,
    defaultCdnHeaders,
    realIpModeOptions,
    proxyStatusById,
    strmMappingPlaceholder,
    addServer,
    validateSettings,
    saveSettings,
    isServerExpanded,
    toggleServerExpanded,
    toggleProxyServer,
    restartProxyServer,
    removeServer,
    isApiKeyVisible,
    isApiKeyRevealLoading,
    apiKeyInputValue,
    apiKeyPlaceholder,
    updateApiKeyInput,
    toggleApiKeyVisible,
    needsRealIpHeader,
    updateRealIpMode,
    validationClass,
    localizeValidationText,
  }
}
