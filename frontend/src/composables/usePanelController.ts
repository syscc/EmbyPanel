import type * as Forge from 'node-forge'
import { Archive, Bell, FileText, LayoutDashboard, Server, UserRound, Users } from '@lucide/vue'
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'

import { useRoute, useRouter } from 'vue-router'
import type {
  Settings,
  RealIpMode,
  EmbyServerConfig,
  PublicKeyResponse,
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
  EncryptionPublicKey,
} from '@/types/panel'

export function usePanelController() {
  const tokenKey = 'embypanel_token'
  const pageKey = 'embypanel_page'
  const themeKey = 'embypanel_theme'
  const localeKey = 'embypanel_locale'
  const validPages: Page[] = ['home', 'server', 'clients', 'logs', 'notifications', 'backup', 'account']
  const mode = ref<AuthMode>('loading')
  const route = useRoute()
  const router = useRouter()
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
  const publicKey = ref<EncryptionPublicKey | null>(null)
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
  const expandedServerCards = ref<Record<string, boolean>>({})
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
  let noticeTimer: number | undefined
  let pageActivation = 0
  let pagePollRunning = false
  let logRequestId = 0
  let auditRequestId = 0
  let dataSession = 0
  let accountSaveOperation = 0
  let settingsLoaded = false
  let clientControlLoaded = false
  let logConfigLoaded = false
  let settingsRequest: Promise<void> | undefined
  let clientControlRequest: Promise<void> | undefined
  let logConfigRequest: Promise<void> | undefined
  let forgeRequest: Promise<typeof import('node-forge')> | undefined

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

  const translations: Record<string, string> = {
    '加载中': 'Loading',
    '首次初始化': 'First-time setup',
    '管理员登录': 'Administrator sign in',
    '用户名': 'Username',
    '密码': 'Password',
    '处理中': 'Working',
    '创建并进入': 'Create and enter',
    '登录': 'Sign in',
    '首页': 'Overview',
    '服务器': 'Servers',
    '客户端': 'Clients',
    '日志': 'Logs',
    '通知': 'Notifications',
    '备份': 'Backups',
    '账户': 'Account',
    '退出登录': 'Sign out',
    '登出': 'Sign out',
    '切换到浅色模式': 'Switch to light mode',
    '切换到暗夜模式': 'Switch to dark mode',
    '浅色模式': 'Light mode',
    '暗夜模式': 'Dark mode',
    '中文': 'Chinese',
    '英文': 'English',
    '简体中文': 'Simplified Chinese',
    '切换语言': 'Change language',
    'English': 'English',
    '检查更新': 'Check for updates',
    '正在检查更新': 'Checking for updates',
    '点击检查更新': 'Click to check for updates',
    '有新版本': 'New version available',
    '检查失败': 'Update check failed',
    '版本读取中': 'Reading version',
    '检查中': 'Checking',
    '有更新': 'Update available',
    '最新': 'Up to date',
    '失败': 'Failed',
    '媒体库总览': 'Media overview',
    '在线': 'Online',
    '电影': 'Movies',
    '剧集': 'Series',
    '总集数': 'Episodes',
    '集数': 'Episodes',
    '个媒体库': 'libraries',
    '用户': 'Users',
    '服务器状态': 'System health',
    '运行': 'Uptime',
    '内存': 'Memory',
    '磁盘': 'Disk',
    '核': 'cores',
    '运维概览': 'Operations',
    '健康检查、反代监听和今日请求统计。': 'Health checks, proxy listeners, and today\'s traffic.',
    '刷新': 'Refresh',
    '刷新中': 'Refreshing',
    '今日请求': 'Requests today',
    '重定向': 'Redirects',
    '缓存命中': 'Cache hits',
    '拦截 / 错误': 'Blocks / errors',
    '健康': 'Healthy',
    '待检查': 'Needs check',
    'STRM 直链': 'STRM direct links',
    '内存直链缓存': 'In-memory direct-link cache',
    '今日累计': 'Today total',
    '端口': 'Port',
    '最近请求': 'Last request',
    '最近巡检': 'Last check',
    '耗时': 'Latency',
    '反代': 'Proxy',
    '最近自动重启': 'Last automatic restart',
    '播放频率限制': 'Playback rate limit',
    '活跃窗口': 'Active windows',
    '已封禁': 'Blocked',
    '最高命中': 'Peak count',
    '实时播放': 'Live playback',
    '转码': 'Transcoding',
    '直放': 'Direct play',
    '服务器配置': 'Server configuration',
    '添加服务器': 'Add server',
    '测试配置': 'Test configuration',
    '保存配置': 'Save configuration',
    '保存中': 'Saving',
    '收起配置': 'Collapse',
    '展开配置': 'Configure',
    '关闭服务器': 'Disable server',
    '开启服务器': 'Enable server',
    '重启服务器': 'Restart server',
    '重启中': 'Restarting',
    '删除': 'Delete',
    '名称': 'Name',
    'Emby 地址': 'Emby address',
    '反代端口': 'Proxy port',
    '真实 IP 获取方式': 'Client IP strategy',
    '缓存秒数': 'Cache TTL (seconds)',
    '缓存最大条数': 'Cache capacity',
    '缓存与巡检': 'Cache and monitoring',
    '直链与重定向': 'Direct links and redirects',
    'OpenList 地址': 'OpenList address',
    'OpenList Token': 'OpenList token',
    '客户端管控': 'Client controls',
    '通知配置': 'Notification settings',
    '配置备份': 'Configuration backup',
    '账户资料': 'Profile',
    '账户与安全': 'Account and security',
    '用户名留空保持不变；密码字段全部留空时不修改密码。': 'Leave the username blank to keep it unchanged; leave all password fields blank to keep the current password.',
    '留空则保持当前用户名不变。': 'Leave blank to keep the current username.',
    '修改密码': 'Change password',
    '密码字段全部留空时不会修改密码。': 'Leave all password fields blank to keep the current password.',
    '配置审计': 'Configuration audit',
    '保存': 'Save',
    '导出 CSV': 'Export CSV',
    '筛选': 'Filter',
    '搜索': 'Search',
    '添加 Webhook': 'Add webhook',
    '命中通知': 'Match notifications',
    '启用': 'Enabled',
    '密钥（可选）': 'Secret (optional)',
    'Webhook 使用 POST JSON 发送：{ title, text }。命中通知包含播放频率屏蔽和 UA 拦截命中。': 'Webhooks send POST JSON with { title, text } for rate-limit and UA block events.',
    '例如：企业微信通知': 'e.g. Operations notification',
    '可选密钥': 'Optional secret',
    '请求体固定为 {"title":"${title}","text":"${text}"}，密钥会通过 `X-Webhook-Secret` 头发送。': 'The request body is {"title":"${title}","text":"${text}"}; the secret is sent in the X-Webhook-Secret header.',
    '导出备份': 'Export',
    '还原': 'Restore',
    '关闭': 'Close',
    '成功': 'Success',
    '警告': 'Warning',
    '错误': 'Error',
    '信息': 'Info',
    '正常': 'Normal',
    '异常': 'Issue',
    '未启动': 'Stopped',
    '未启用': 'Disabled',
    '监听中': 'Listening',
    '未监听': 'Not listening',
    '未巡检': 'Not checked',
    '未配置': 'Not configured',
    '无失败': 'No failures',
    '暂无详情': 'No details',
    '暂无审计记录。': 'No audit records.',
    '主导航': 'Primary navigation',
    '系统在线': 'System online',
    '打开导航': 'Open navigation',
    '播放日志': 'Playback logs',
    '拦截日志': 'Blocked logs',
    '反代请求': 'Proxy requests',
    '运行日志': 'Runtime logs',
    '实时刷新': 'Live refresh',
    '日志文件配置': 'Log file settings',
    '系统识别': 'Automatic detection',
    '从 HTTP Header 中获取': 'Read from an HTTP header',
    '从 Header 列表中获取': 'Read from a header list',
    '获取 X-Forwarded-For 的上一级代理地址': 'Use the previous X-Forwarded-For hop',
    '获取 X-Forwarded-For 的上上一级代理地址': 'Use the second previous X-Forwarded-For hop',
    '获取 X-Forwarded-For 的上上上一级代理地址': 'Use the third previous X-Forwarded-For hop',
    '屏蔽 IP': 'Block IP',
    '禁用用户': 'Disable user',
    '混合模式': 'Mixed mode',
    '屏蔽频繁播放的 IP': 'Block high-frequency playback IPs',
    '通过 API 禁用该用户': 'Disable the user through the API',
    '同时禁用用户并屏蔽频繁播放的 IP': 'Disable the user and block the IP',
    '全部级别': 'All levels',
    '已拦截': 'Blocked',
    '封禁变更': 'Ban changes',
    'REDIRECT - 直链/跳转': 'REDIRECT - direct link / redirect',
    'CACHE - 缓存命中': 'CACHE - cache hit',
    'WARNING - 警告': 'WARNING - warning',
    'BLOCKED - 已拦截': 'BLOCKED - blocked',
    'INFO - 信息': 'INFO - info',
    'SUCCESS - 成功': 'SUCCESS - success',
    'ERROR - 错误': 'ERROR - error',
    'DEBUG - 调试': 'DEBUG - debug',
    'CRITICAL - 严重': 'CRITICAL - critical',
    '正在读取媒体库总览': 'Reading media overview',
    '正在读取服务器状态': 'Reading system health',
    '首页直接查看当前窗口命中和封禁情况。': 'Monitor active windows and blocks at a glance.',
    '当前监控中的 IP': 'IPs currently being monitored',
    '屏蔽 IP / 禁用用户': 'Blocked IPs / disabled users',
    '当前窗口最大次数': 'Highest count in the current window',
    '封禁方式': 'Action',
    '命中': 'Hits',
    '窗口': 'Window',
    '状态': 'Status',
    '条': 'records',
    '解除封禁': 'Unblock',
    '观察中': 'Watching',
    '当前没有播放频率窗口数据。': 'No playback rate windows.',
    '正在读取 Emby 播放会话': 'Reading Emby sessions',
    '当前没有正在播放的媒体': 'No media is playing',
    '每个 Emby 服务器使用独立反代端口，保存后自动监听。': 'Each Emby server uses an independent proxy port and starts listening after save.',
    '例如：主服务器': 'e.g. Main server',
    '启动': 'Started',
    '隐藏 Emby API Key': 'Hide Emby API key',
    '显示 Emby API Key': 'Show Emby API key',
    '例如：x-real-ip': 'e.g. x-real-ip',
    '从下列常用 CDN 携带真实 IP 的 HTTP Header 中获取，按顺序取第一个能获取到的值。': 'Check the common CDN client-IP headers below and use the first available value.',
    '默认使用系统识别。经过 CDN 或多层反代后 IP 不准时再配置，保存后会同步重启对应反代服务。': 'Use automatic detection by default. Configure this only when a CDN or proxy chain reports the wrong IP.',
    '可选': 'Optional',
    '启用服务器连通性巡检': 'Enable connectivity checks',
    '巡检间隔秒数': 'Check interval (seconds)',
    '单项超时秒数': 'Check timeout (seconds)',
    '反代无响应自动重启秒数': 'Proxy auto-restart delay (seconds)',
    '填 0 表示不自动重启；只在反代端口连续无响应时触发。': 'Use 0 to disable automatic restarts. It only triggers after continuous proxy failures.',
    '缓存过滤模式': 'Cache filter mode',
    '不过滤': 'No filter',
    '白名单：命中才缓存': 'Allowlist: cache matches only',
    '黑名单：命中不缓存': 'Blocklist: skip matching hosts',
    '缓存过滤域名': 'Cache host filters',
    '支持多个域名、通配符或关键字；每行一个，例如：*.115cdn.* 或 115': 'One host, wildcard, or keyword per line, such as *.115cdn.* or 115',
    '只匹配直链域名部分。白名单命中才缓存；黑名单命中不缓存，其他直链正常缓存。': 'Filters match direct-link hosts only. Allowlist matches are cached; blocklist matches are skipped.',
    '开启内部重定向 HEAD 解析': 'Resolve internal redirects with HEAD',
    'HEAD 超时秒数': 'HEAD timeout (seconds)',
    'STRM URL 映射': 'STRM URL mappings',
    '配置测试结果': 'Configuration test results',
    '重新测试': 'Run again',
    '还没有运行配置测试。': 'No configuration test has run yet.',
    '自动记录播放设备和 UA，也可以按播放频率临时禁用账号。': 'Track playback devices and user agents, with temporary rate-based blocks.',
    '启用 UA 拦截': 'Enable UA blocking',
    '启用播放频率限制': 'Enable playback rate limiting',
    '启用同时播放限制': 'Enable concurrent playback limit',
    '屏蔽方式': 'Block action',
    '封禁原因': 'Block reason',
    '同时播放超限': 'Concurrent playback limit',
    '到期时间': 'Expires',
    '检测时间窗口（秒）': 'Detection window (seconds)',
    '最大播放次数': 'Maximum play requests',
    '封禁时长（秒）': 'Block duration (seconds)',
    '允许同时播放数': 'Concurrent play limit',
    '当前次数': 'Current count',
    '阈值': 'Threshold',
    '剩余': 'Remaining',
    '重置时间': 'Resets',
    '暂无频率限制封禁。': 'No active rate-limit blocks.',
    '播放频率窗口': 'Playback rate windows',
    '显示当前检测窗口内各 IP 的播放请求计数。': 'Request counts for each IP in the current detection window.',
    '客户端记录': 'Client records',
    '手动添加 UA 拦截': 'Add a UA block',
    '输入 UA 完整内容或关键字，保存后默认进入禁用状态。': 'Enter a full UA or keyword. New rules start in blocked mode.',
    'UA 关键字': 'UA keyword',
    '描述': 'Description',
    '例如：Infuse / Fileball / okhttp': 'e.g. Infuse / Fileball / okhttp',
    '例如：临时禁用某客户端': 'e.g. Temporarily block a client',
    '日期说明': 'Date reference',
    '时间为该 UA 第一次出现或手动添加的时间；后台更新时间只在规则状态、备注或客户端信息变化时刷新，同一 UA 重复请求不会每次刷新。': 'The date is when this UA first appeared or was added. Repeated requests do not continually update it.',
    '关键字': 'Keyword',
    '记录时间': 'Recorded at',
    '操作': 'Actions',
    '自动记录播放设备': 'Automatically detected playback device',
    '手动 UA 拦截': 'Manual UA block',
    '关闭 UA 拦截': 'Disable UA blocking',
    '开启 UA 拦截': 'Enable UA blocking',
    '当前筛选没有匹配的客户端。': 'No clients match the current filters.',
    '暂无客户端记录，开始播放后会自动出现，也可以手动添加 UA 拦截。': 'No client records yet. Playback devices appear automatically, or you can add a UA block.',
    '添加拦截': 'Add block',
    '添加中': 'Adding',
    '搜索 UA': 'Search UA',
    '清空筛选': 'Clear filters',
    '全部': 'All',
    '已禁用': 'Disabled',
    '允许播放': 'Allowed',
    '保存通知': 'Save notifications',
    '测试连接': 'Test connection',
    '测试中': 'Testing',
    '配置文件备份 / 还原': 'Backup / restore',
    '导出配置文件，或从电脑选择配置文件还原运行配置。': 'Export a configuration file or restore one from this device.',
    '备份范围': 'Backup scope',
    'Emby 地址、API Key、反代端口、真实 IP、缓存和映射规则': 'Emby address, API key, proxy port, client IP, cache, and mapping rules',
    'UA 拦截、播放频率限制、封禁列表和客户端规则': 'UA blocks, playback limits, ban lists, and client rules',
    'Webhook 地址、启用状态和密钥': 'Webhook URLs, enabled state, and secrets',
    '日志配置': 'Log settings',
    '日志级别、文件大小、保留数量和格式': 'Log level, file size, retention count, and format',
    '备份文件会使用备份密码加密；不包含面板管理员用户名、密码、登录会话、运行日志文件和请求统计数据。': 'Backups are encrypted with your password and exclude administrator credentials, sessions, runtime logs, and request statistics.',
    '输入备份密码后生成加密的 `embypanel-config-时间.json` 并弹出浏览器下载。': 'Enter a password to create and download an encrypted embypanel-config timestamp file.',
    '点击后选择本机配置文件，加密备份需要输入对应密码，读取成功后自动还原并重启反代服务。': 'Choose a local configuration file. Encrypted backups require their password and restart proxy services after restore.',
    '日志类型': 'Log type',
    '单列表查看播放日志、拦截日志、反代请求和运行日志，页面打开时每 3 秒自动刷新。': 'Review playback, blocked, proxy, and runtime logs in one stream with a three-second live refresh.',
    '级别': 'Level',
    '全部服务器': 'All servers',
    '请求类型': 'Request type',
    '全部请求': 'All requests',
    '关键词': 'Keyword',
    '搜索用户 / IP / URL / 信息': 'Search user / IP / URL / message',
    '开始时间': 'Start time',
    '结束时间': 'End time',
    '加载更多': 'Load more',
    '单次最多': 'Up to',
    '保留': 'retained for',
    '天或最近': 'days or the latest',
    '内存最多保留最近': 'Memory retains the latest',
    '条可视化日志': 'visual log records',
    '点击复制链接': 'Click to copy link',
    '未命中': 'Miss',
    '已解除': 'Unblocked',
    '未拦截': 'Not blocked',
    '已显示': 'Showing',
    '暂无': 'No',
    '保存日志配置': 'Save log settings',
    '日志级别': 'Log level',
    '单文件最大 MB': 'Maximum file size (MB)',
    '保留文件数': 'Files to retain',
    '日志格式': 'Log format',
    '日志写入 data/logs/embypanel.log，默认 INFO 级别。': 'Logs are written to data/logs/embypanel.log at INFO level by default.',
    '操作类型': 'Action type',
    '记录配置、账户、通知、备份恢复等管理操作，不保存敏感明文。': 'Tracks configuration, account, notification, and restore actions without storing sensitive plaintext.',
    '当前密码': 'Current password',
    '新密码': 'New password',
    '确认新密码': 'Confirm new password',
    '全部操作': 'All actions',
    '搜索管理员 / 操作 / 摘要': 'Search admin / action / summary',
    '审计记录加载失败': 'Unable to load audit records',
    '链接已复制': 'Link copied',
    '复制失败，请手动复制': 'Copy failed. Copy it manually.',
    '配置测试通过': 'Configuration test passed',
    '配置测试完成，请查看警告项': 'Configuration test finished. Review warnings.',
    '日志配置已保存': 'Log settings saved',
    '账户资料已更新': 'Profile updated',
    '客户端管控规则已保存': 'Client control rules saved',
    'Webhook URL 不能为空': 'Webhook URL is required',
    'Webhook 测试发送成功': 'Webhook test sent',
    'EmbyPanel 通知测试': 'EmbyPanel notification test',
    'Webhook POST 测试成功': 'Webhook POST test succeeded',
    '新建 Webhook': 'New webhook',
    'UA 关键字不能为空': 'UA keyword is required',
    'UA 拦截规则已添加': 'UA block added',
    '新密码不能为空': 'New password is required',
    '当前密码不能为空': 'Current password is required',
    '当前密码不正确': 'The current password is incorrect',
    '确认新密码不能为空': 'Confirm your new password',
    '两次输入的新密码不一致': 'The new passwords do not match',
    '管理员密码已更新': 'Administrator password updated',
    '保存账户设置': 'Save account settings',
    '账户设置已更新': 'Account settings updated',
    '未检测到需要保存的修改': 'No changes to save',
    '密码已更新，但用户名修改失败': 'The password was updated, but the username change failed',
    'UA 规则已删除': 'UA rule deleted',
    '登录已过期，请重新登录': 'Your session expired. Sign in again.',
    '页面加载失败，请刷新后重试': 'Unable to load the page. Refresh and try again.',
    '重试': 'Retry',
    '加密请求失败': 'Unable to encrypt the request',
    '反代服务已重启': 'Proxy service restarted',
    '已开启': 'enabled',
    '已关闭': 'disabled',
    '确定删除这个服务器配置吗？对应反代端口保存后会停止监听。': 'Delete this server configuration? Its proxy port will stop listening after save.',
    '服务器配置已保存，反代服务已重启': 'Server configuration saved. Proxy services restarted.',
    '请输入备份密码（至少 4 位），用于加密配置文件': 'Enter a backup password (at least 4 characters).',
    '备份密码至少需要 4 位': 'Backup passwords need at least 4 characters.',
    '加密配置备份已生成，请妥善保存备份密码': 'Encrypted backup created. Keep the password safe.',
    '配置文件内容为空': 'The configuration file is empty.',
    '还原配置文件会覆盖当前配置并重启反代服务，确定继续吗？': 'Restoring will overwrite the current configuration and restart proxy services. Continue?',
    '请输入该加密备份的密码': 'Enter the password for this encrypted backup.',
    '加密备份密码不能为空': 'The encrypted backup password cannot be empty.',
    '配置文件已还原，反代服务已重启': 'Configuration restored. Proxy services restarted.',
    '用户封禁已解除': 'User ban lifted',
    '混合封禁已解除': 'Mixed ban lifted',
    'IP 屏蔽已解除': 'IP block lifted',
    '封禁动作': 'Ban action',
    '封禁': 'Block',
    '解除封禁动作': 'Unblock action',
    '缓存': 'Cache',
    '跳转': 'Redirect',
    '视频流': 'Video stream',
    '播放信息': 'Playback info',
    '系统信息': 'System info',
    '播放器脚本': 'Player script',
    '普通代理': 'Standard proxy',
    '配置': 'Configuration',
    '本地配置校验通过': 'Local configuration passed',
    '本地配置校验失败': 'Local configuration failed',
    '反代端口重复': 'Duplicate proxy port',
    '反代端口正在由当前服务监听': 'The current service is listening on this proxy port',
    '反代端口可用': 'Proxy port is available',
    '反代端口已被占用': 'Proxy port is already in use',
    'Emby API Key 可用': 'Emby API key is valid',
    'Emby 连接失败': 'Emby connection failed',
    'OpenList 连接可用': 'OpenList connection is available',
    'OpenList 连接失败': 'OpenList connection failed',
    '未配置，已跳过': 'Not configured; skipped',
  }

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
    connectivity_check_enabled: true,
    connectivity_check_interval_seconds: 60,
    connectivity_check_timeout_seconds: 5,
    connectivity_auto_restart_seconds: 180,
  })

  const menu = [
    { id: 'home' as const, label: '首页', icon: LayoutDashboard },
    { id: 'server' as const, label: '服务器', icon: Server },
    { id: 'clients' as const, label: '客户端', icon: Users },
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

  function isHttpUrl(value: string): boolean {
    const trimmed = value.trim()
    return trimmed.startsWith('http://') || trimmed.startsWith('https://')
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
      showNotice(t('服务器配置已保存，反代服务已重启'))
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

  async function exportBackup() {
    backupError.value = ''
    clearNotice()
    const password = window.prompt(t('请输入备份密码（至少 4 位），用于加密配置文件'))
    if (password === null) return
    if (password.trim().length < 4) {
      backupError.value = t('备份密码至少需要 4 位')
      return
    }
    try {
      const response = await api<{ backup: string }>('/api/settings/backup/export', {
        method: 'POST',
        body: JSON.stringify(await encryptPayload('backup_export', { password })),
      })
      downloadTextFile(response.backup, backupFileName())
      showNotice(t('加密配置备份已生成，请妥善保存备份密码'))
    } catch (err) {
      backupError.value = err instanceof Error ? err.message : String(err)
    }
  }

  async function importBackup() {
    backupError.value = ''
    clearNotice()
    backupFileInput.value?.click()
  }

  async function handleBackupFileSelected(event: Event) {
    const input = event.target as HTMLInputElement
    const file = input.files?.[0]
    input.value = ''
    if (!file) return
    backupError.value = ''
    clearNotice()
    try {
      const backup = await file.text()
      await importBackupText(backup)
    } catch (err) {
      backupError.value = err instanceof Error ? err.message : String(err)
    }
  }

  async function importBackupText(backupText: string) {
    backupError.value = ''
    clearNotice()
    const backup = backupText.trim()
    if (!backup) {
      backupError.value = t('配置文件内容为空')
      return
    }
    const confirmed = window.confirm(t('还原配置文件会覆盖当前配置并重启反代服务，确定继续吗？'))
    if (!confirmed) return
    const encryptedBackup = backup.startsWith('{') && backup.includes('"cipher"')
    const password = encryptedBackup ? window.prompt(t('请输入该加密备份的密码')) : null
    if (encryptedBackup && password === null) return
    const backupPassword = password?.trim() || ''
    if (encryptedBackup && !backupPassword) {
      backupError.value = t('加密备份密码不能为空')
      return
    }
    try {
      const response = await api<Settings>('/api/settings/backup/import', {
        method: 'POST',
        body: JSON.stringify(await encryptPayload('backup', { backup, password: backupPassword || null })),
      })
      resetResourceCache()
      Object.assign(settings, response)
      normalizeSettingsServers()
      settingsLoaded = true
      showNotice(t('配置文件已还原，反代服务已重启'))
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
        real_ip_mode: server.real_ip_mode || 'auto',
        real_ip_header: server.real_ip_header.trim(),
      }))
      .filter((server) => server.emby_host || server.emby_api_key)
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
      real_ip_mode: server.real_ip_mode || 'auto',
      real_ip_header: server.real_ip_header || '',
    }))
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
      real_ip_mode: 'auto',
      real_ip_header: '',
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
    const confirmed = window.confirm(t('确定删除这个服务器配置吗？对应反代端口保存后会停止监听。'))
    if (!confirmed) return
    settings.servers = settings.servers.filter((server) => server.id !== serverId)
    const { [serverId]: _removed, ...visibleServers } = visibleApiKeyServers.value
    visibleApiKeyServers.value = visibleServers
    const { [serverId]: _collapsed, ...expandedServers } = expandedServerCards.value
    expandedServerCards.value = expandedServers
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
    const operationToken = token.value
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
        await api<{ changed: boolean }>('/api/change-password', {
          method: 'POST',
          body: JSON.stringify(encryptedPassword),
        })
        if (!operationIsCurrent()) return
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
    const bytes = randomBytes(8)
    return `webhook-${bytesToBase64Url(bytes)}`
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
    const confirmed = window.confirm(
      locale.value === 'en-US'
        ? `Delete the UA rule "${clientKeyword(record)}"?`
        : `确定删除 UA 规则「${clientKeyword(record)}」吗？`,
    )
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
    if (action === 'disable_user') return t('禁用用户')
    if (action === 'mixed') return t('混合模式')
    if (action === 'block_ip') return t('屏蔽 IP')
    return '--'
  }

  function playbackRateUnblockNotice(action?: string) {
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

  function logout() {
    accountSaveOperation += 1
    savingAccount.value = false
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

  async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
    const requestToken = token.value
    const headers = new Headers(init.headers)
    headers.set('Content-Type', 'application/json')
    if (requestToken) headers.set('Authorization', `Bearer ${requestToken}`)
    const response = await fetch(path, { ...init, headers })
    if (!response.ok) {
      const message = await response.text()
      if (response.status === 401 && requestToken && !isAuthBootstrapPath(path)) {
        handleAuthExpired(requestToken)
      }
      throw new Error(message)
    }
    return response.json() as Promise<T>
  }

  function isAuthBootstrapPath(path: string) {
    return path === '/api/login' || path === '/api/setup' || path === '/api/setup-status'
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

  async function fetchPublicKey() {
    const response = await api<PublicKeyResponse>('/api/public-key')
    if (!hasWebCrypto()) {
      const forge = await loadForge()
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
        : await encryptWithForge(publicKey.value.key, aesKey, iv, plaintext)
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

  async function encryptWithForge(
    key: Forge.pki.rsa.PublicKey,
    aesKey: Uint8Array,
    iv: Uint8Array,
    plaintext: Uint8Array,
  ) {
    const forge = await loadForge()
    const encryptedKey = key.encrypt(bytesToBinary(aesKey), 'RSA-OAEP', {
      md: forge.md.sha256.create(),
      mgf1: {
        md: forge.md.sha256.create(),
      },
    })
    const cipher = forge.cipher.createCipher('AES-GCM', bytesToBinary(aesKey))
    cipher.start({ iv: bytesToBinary(iv), tagLength: 128 })
    cipher.update(forge.util.createBuffer(bytesToBinary(plaintext)))
    if (!cipher.finish()) throw new Error(t('加密请求失败'))
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
    throw new Error(t('加密请求失败'))
  }

  function loadForge() {
    forgeRequest ??= import('node-forge')
    return forgeRequest
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
    if (locale.value === 'en-US') {
      return days > 0 ? `${days}d ${hours}h` : `${hours}h`
    }
    return days > 0 ? `${days}天${hours}小时` : `${hours}小时`
  }

  function formatTimestamp(value: string) {
    const date = parseUnixTimestamp(value)
    if (!date) return '--'
    return date.toLocaleString(locale.value)
  }

  function parseUnixTimestamp(value: string) {
    const timestamp = Number(value)
    if (!Number.isFinite(timestamp) || timestamp <= 0) return null
    return new Date(timestamp * 1000)
  }

  function formatLogTime(value: number) {
    if (!Number.isFinite(value) || value <= 0) return '--'
    return new Date(value).toLocaleString(locale.value)
  }

  function formatTimestampMs(value: number | null | undefined) {
    if (!value || !Number.isFinite(value)) return '--'
    return new Date(value).toLocaleString(locale.value)
  }

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

  function requestOutcomeClass(row: ProxyRequestDetail) {
    if (row.event_type === 'unblock') return 'ok'
    if (row.blocked) return 'blocked'
    if (row.cache_hit) return 'cache'
    if (row.status_code >= 500) return 'error'
    if (row.status_code >= 400) return 'warn'
    if (row.status_code >= 300) return 'redirect'
    return 'ok'
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

  watch(page, (nextPage) => {
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
    toggleApiKeyVisible,
    needsRealIpHeader,
    updateRealIpMode,
    validationClass,
    localizeValidationText,
  }
}
