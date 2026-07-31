<script setup lang="ts">
import * as forge from 'node-forge'
import {
  Activity,
  Archive,
  Bell,
  Check,
  ChevronDown,
  ChevronRight,
  CircleCheck,
  Clock3,
  Database,
  Download,
  Eye,
  EyeOff,
  FileText,
  Gauge,
  LayoutDashboard,
  Languages,
  LogOut,
  Menu as MenuIcon,
  Moon,
  PlayCircle,
  Plus,
  RefreshCw,
  RotateCw,
  Server,
  Settings2,
  ShieldCheck,
  SlidersHorizontal,
  Sun,
  Trash2,
  Upload,
  UserRound,
  Users,
  Webhook,
  X,
  Zap,
} from '@lucide/vue'
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'

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
  connectivity_check_enabled: boolean
  connectivity_check_interval_seconds: number
  connectivity_check_timeout_seconds: number
  connectivity_auto_restart_seconds: number
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
  item_id: string
  user_name: string
  client: string
  device_name: string
  user_agent: string
  playback_ip: string | null
  ip_location?: IpLocation
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

type ConnectivityCheckStatus = {
  server_id: string
  server_name: string
  port: number
  enabled: boolean
  ok: boolean
  emby_ok: boolean
  openlist_ok: boolean | null
  proxy_ok: boolean
  checked_at_ms: number
  duration_ms: number
  failed_since_ms: number | null
  auto_restarted_at_ms: number | null
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

type IpLocation = {
  country_name: string
  region_name: string
  city_name: string
  district_name: string
  isp_domain: string
}

type ProxyRequestDetail = {
  id: number
  event_type?: 'request' | 'block' | 'unblock'
  timestamp_ms: number
  server_id: string
  server_name: string
  port: number
  method: string
  path: string
  path_type: string
  status_code: number
  outcome: string
  duration_ms: number
  playback_user: string
  playback_ip: string
  ip_location?: IpLocation
  cache_hit: boolean
  blocked: boolean
  detail: string
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
  action: 'block_ip' | 'disable_user' | 'mixed'
  ip: string
  ip_location?: IpLocation
  user_name: string
  blocked_until: string
  created_at: string
  enabled: boolean
  note: string
}

type PlaybackRateWindowStatus = {
  server_id: string
  block_id?: string
  block_action?: string
  user_name: string
  ip: string
  ip_location?: IpLocation
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
  playback_rate_limit_action: 'block_ip' | 'disable_user' | 'mixed'
  concurrent_playback_limit_enabled: boolean
  concurrent_playback_limit_max: number
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
  ip_location?: IpLocation
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
type LogViewFilter = 'playback' | 'blocked' | 'proxy' | 'general'
type Locale = 'zh-CN' | 'en-US'
type EncryptionPublicKey =
  | { kind: 'webcrypto'; key: CryptoKey }
  | { kind: 'forge'; key: forge.pki.rsa.PublicKey }

const tokenKey = 'embypanel_token'
const pageKey = 'embypanel_page'
const themeKey = 'embypanel_theme'
const localeKey = 'embypanel_locale'
const validPages: Page[] = ['home', 'server', 'clients', 'logs', 'notifications', 'backup', 'account']
const mode = ref<AuthMode>('loading')
const page = ref<Page>(storedPage())
const token = ref(storedToken())
const darkMode = ref(storedTheme() === 'dark')
const locale = ref<Locale>(storedLocale())
const localeMenuOpen = ref(false)
const mobileNavOpen = ref(false)
const mobileMenuButton = ref<HTMLButtonElement | null>(null)
const mobileNavCloseButton = ref<HTMLButtonElement | null>(null)
const mobileSidebar = ref<HTMLElement | null>(null)
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
let logsTimer: number | undefined
let noticeTimer: number | undefined

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
  'OpenList 地址': 'OpenList address',
  'OpenList Token': 'OpenList token',
  '客户端管控': 'Client controls',
  '通知配置': 'Notification settings',
  '配置备份': 'Configuration backup',
  '账户资料': 'Profile',
  '修改密码': 'Change password',
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
  '保存资料': 'Save profile',
  '修改中': 'Updating',
  '当前密码': 'Current password',
  '新密码': 'New password',
  '确认新密码': 'Confirm new password',
  '全部操作': 'All actions',
  '搜索管理员 / 操作 / 摘要': 'Search admin / action / summary',
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
  '两次输入的新密码不一致': 'The new passwords do not match',
  '管理员密码已更新': 'Administrator password updated',
  'UA 规则已删除': 'UA rule deleted',
  '登录已过期，请重新登录': 'Your session expired. Sign in again.',
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
    ? 'One mapping per line: source => target\nhttps://source.example.com => http://media-gateway.local:5244\nRegex: regex:https://source\\.(example|test)\\.com => http://media-gateway.local:5244'
    : '每行一个映射：原地址 => 新地址\nhttps://source.example.com => http://media-gateway.local:5244\n高级正则：regex:https://source\\.(example|test)\\.com => http://media-gateway.local:5244',
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
  page.value = nextPage
  closeMobileNav()
  localeMenuOpen.value = false
  writeStorage(localStorage, pageKey, nextPage)
  if (nextPage === 'logs') void refreshActivityLogs()
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
  localeMenuOpen.value = false
}

function toggleLocaleMenu() {
  localeMenuOpen.value = !localeMenuOpen.value
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
  void refreshUpdateCheck(false)
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

async function refreshClientControlLiveData() {
  try {
    const response = await api<ClientControlConfig>('/api/client-control')
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
    showNotice(t('服务器配置已保存，反代服务已重启'))
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
    Object.assign(settings, response)
    normalizeSettingsServers()
    await refreshOperationalData()
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
  logsError.value = ''
  clearNotice()
  try {
    Object.assign(logConfig, await api<SystemLogConfig>('/api/settings/log-config', {
      method: 'PUT',
      body: JSON.stringify(await encryptPayload('log_config', { ...logConfig })),
    }))
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
    showNotice(`${server.name || t('服务器')} ${nextEnabled ? t('已开启') : t('已关闭')}`)
    await refreshOperationalData()
    await refreshDashboard()
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

async function saveProfile() {
  savingProfile.value = true
  clearNotice()
  error.value = ''
  try {
    const response = await api<Profile>('/api/profile', {
      method: 'PUT',
      body: JSON.stringify(await encryptPayload('profile', { username: profileForm.username })),
    })
    Object.assign(profile, response)
    profileForm.username = response.username
    showNotice(t('账户资料已更新'))
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
  } finally {
    savingProfile.value = false
  }
}

async function saveClientControl() {
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

async function changePassword() {
  error.value = ''
  clearNotice()
  if (!passwordForm.new_password) {
    error.value = t('新密码不能为空')
    return
  }
  if (passwordForm.new_password !== passwordForm.confirm_password) {
    error.value = t('两次输入的新密码不一致')
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
    showNotice(t('管理员密码已更新'))
  } catch (err) {
    error.value = err instanceof Error ? err.message : String(err)
  } finally {
    changingPassword.value = false
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
  token.value = ''
  page.value = 'home'
  mobileNavOpen.value = false
  localeMenuOpen.value = false
  clearStoredToken()
  removeStorage(localStorage, pageKey)
  stopDashboardPolling()
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
    if (page.value === 'clients') await refreshClientControlLiveData()
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
    if (selectedLogView.value === 'playback') {
      activityLogs.value = await fetchActivityLogs('playback', playbackLogLimit.value)
      proxyRequestDetails.value = []
    } else if (selectedLogView.value === 'general') {
      activityLogs.value = await fetchActivityLogs('general', generalLogLimit.value)
      proxyRequestDetails.value = []
    } else {
      proxyRequestDetails.value = await fetchProxyRequestDetails()
      activityLogs.value = []
    }
  } catch (err) {
    activityLogs.value = []
    proxyRequestDetails.value = []
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
  clearNotice()
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
  mobileNavOpen.value = false
  localeMenuOpen.value = false
  clearStoredToken()
  stopDashboardPolling()
  mode.value = 'login'
  error.value = t('登录已过期，请重新登录')
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

onMounted(() => {
  document.documentElement.lang = locale.value
  bootstrap()
})
onBeforeUnmount(stopDashboardPolling)
</script>

<template>
  <main v-if="mode === 'loading'" class="auth-shell" :class="{ dark: darkMode }">
    <div class="auth-atmosphere" aria-hidden="true" />
    <div class="auth-utility">
      <button
        class="icon-button"
        type="button"
        :aria-label="darkMode ? t('切换到浅色模式') : t('切换到暗夜模式')"
        :title="darkMode ? t('浅色模式') : t('暗夜模式')"
        @click="toggleTheme"
      >
        <Sun v-if="darkMode" :size="17" />
        <Moon v-else :size="17" />
      </button>
      <div class="language-picker">
        <button
          class="language-trigger"
          type="button"
          :aria-label="t('切换语言')"
          :aria-expanded="localeMenuOpen"
          @click="toggleLocaleMenu"
        >
          <Languages :size="16" />
          {{ locale === 'zh-CN' ? t('简体中文') : 'English' }}
          <ChevronDown :size="14" />
        </button>
        <div v-if="localeMenuOpen" class="language-menu">
          <button type="button" :class="{ selected: locale === 'zh-CN' }" @click="setLocale('zh-CN')">
            <span>简体中文</span><Check v-if="locale === 'zh-CN'" :size="15" />
          </button>
          <button type="button" :class="{ selected: locale === 'en-US' }" @click="setLocale('en-US')">
            <span>English</span><Check v-if="locale === 'en-US'" :size="15" />
          </button>
        </div>
      </div>
    </div>
    <section class="auth-card loading-card">
      <div class="loading-mark"><Activity :size="22" /></div>
      <span>{{ t('加载中') }}</span>
    </section>
  </main>

  <main v-else-if="mode === 'setup' || mode === 'login'" class="auth-shell" :class="{ dark: darkMode }">
    <div class="auth-atmosphere" aria-hidden="true" />
    <div class="auth-utility">
      <button
        class="icon-button"
        type="button"
        :aria-label="darkMode ? t('切换到浅色模式') : t('切换到暗夜模式')"
        :title="darkMode ? t('浅色模式') : t('暗夜模式')"
        @click="toggleTheme"
      >
        <Sun v-if="darkMode" :size="17" />
        <Moon v-else :size="17" />
      </button>
      <div class="language-picker">
        <button
          class="language-trigger"
          type="button"
          :aria-label="t('切换语言')"
          :aria-expanded="localeMenuOpen"
          @click="toggleLocaleMenu"
        >
          <Languages :size="16" />
          {{ locale === 'zh-CN' ? t('简体中文') : 'English' }}
          <ChevronDown :size="14" />
        </button>
        <div v-if="localeMenuOpen" class="language-menu">
          <button type="button" :class="{ selected: locale === 'zh-CN' }" @click="setLocale('zh-CN')">
            <span>简体中文</span><Check v-if="locale === 'zh-CN'" :size="15" />
          </button>
          <button type="button" :class="{ selected: locale === 'en-US' }" @click="setLocale('en-US')">
            <span>English</span><Check v-if="locale === 'en-US'" :size="15" />
          </button>
        </div>
      </div>
    </div>
    <section class="auth-card">
      <div class="auth-brand">
        <div class="auth-logo-wrap"><img class="logo-mark" :src="logoUrl" alt="Emby Panel" /></div>
        <div>
          <span class="eyebrow">MEDIA CONTROL / 302</span>
          <h1>Emby Panel</h1>
          <p>{{ mode === 'setup' ? t('首次初始化') : t('管理员登录') }}</p>
        </div>
      </div>
      <div v-if="error" class="notice error" role="alert">{{ error }}</div>
      <label>
        <span>{{ t('用户名') }}</span>
        <input v-model="credentials.username" autocomplete="username" />
      </label>
      <label>
        <span>{{ t('密码') }}</span>
        <input
          v-model="credentials.password"
          type="password"
          autocomplete="current-password"
          @keyup.enter="mode === 'setup' ? setupAdmin() : login()"
        />
      </label>
      <button class="primary wide" :disabled="saving" @click="mode === 'setup' ? setupAdmin() : login()">
        {{ saving ? t('处理中') : mode === 'setup' ? t('创建并进入') : t('登录') }}
      </button>
      <p class="auth-footnote">EmbyPanel · {{ appInfo.version || '0.2' }} · secure local console</p>
    </section>
  </main>

  <main v-else class="app-shell" :class="{ dark: darkMode, 'mobile-nav-open': mobileNavOpen }" @keydown.esc="closeMobileNav">
    <div v-if="mobileNavOpen" class="nav-backdrop" aria-hidden="true" @click="closeMobileNav" />
    <aside
      id="primary-navigation"
      ref="mobileSidebar"
      class="sidebar"
      :aria-label="t('主导航')"
      @keydown="trapMobileNavFocus"
    >
      <div class="brand-row compact">
        <div class="brand-logo-wrap"><img class="logo-mark" :src="logoUrl" alt="Emby Panel" /></div>
        <button
          class="brand-version"
          :class="{ update: updateCheck?.has_update, error: Boolean(updateCheckError) }"
          :title="
            updateChecking
              ? t('正在检查更新')
              : updateCheck?.has_update
                ? `${t('有新版本')}：${updateCheck.latest_version}`
                : updateCheckError
                  ? `${t('检查失败')}：${updateCheckError}`
                  : t('点击检查更新')
          "
          @click="refreshUpdateCheck(true)"
        >
          <strong>{{ appInfo.name }}</strong>
          <small>
            {{ appInfo.version || t('版本读取中') }}
            <span v-if="updateChecking" class="brand-version-badge">{{ t('检查中') }}</span>
            <span v-else-if="updateCheck?.has_update" class="brand-version-badge update">{{ t('有更新') }}</span>
            <span v-else-if="updateCheck" class="brand-version-badge latest">{{ t('最新') }}</span>
            <span v-else-if="updateCheckError" class="brand-version-badge error">{{ t('失败') }}</span>
          </small>
        </button>
        <button ref="mobileNavCloseButton" class="sidebar-close icon-button" type="button" :aria-label="t('关闭')" @click="closeMobileNav">
          <X :size="18" />
        </button>
      </div>

      <nav>
        <button
          v-for="item in menu"
          :key="item.id"
          class="nav-item"
          :class="{ active: page === item.id }"
          @click="setPage(item.id)"
        >
          <component :is="item.icon" :size="18" :stroke-width="1.8" />
          <span>{{ t(item.label) }}</span>
          <ChevronRight class="nav-chevron" :size="14" />
        </button>
      </nav>

      <div class="sidebar-footer">
        <div class="sidebar-status"><span class="status-orb" /> <span>{{ t('系统在线') }}</span></div>
        <button class="nav-item logout" @click="logout">
          <LogOut :size="18" :stroke-width="1.8" />
          <span>{{ t('退出登录') }}</span>
        </button>
      </div>
    </aside>

    <section class="workspace">
      <header class="topbar">
        <div class="topbar-leading">
          <button
            ref="mobileMenuButton"
            class="mobile-menu-button icon-button"
            type="button"
            aria-controls="primary-navigation"
            :aria-expanded="mobileNavOpen"
            :aria-label="t('打开导航')"
            @click="toggleMobileNav"
          >
            <MenuIcon v-if="!mobileNavOpen" :size="19" />
            <X v-else :size="19" />
          </button>
          <div class="breadcrumb">
            <span class="breadcrumb-root">EmbyPanel</span>
            <ChevronRight :size="14" />
            <strong>{{ t(menu.find((item) => item.id === page)?.label || '首页') }}</strong>
          </div>
        </div>
        <div class="top-actions">
          <div class="language-picker">
            <button
              class="language-trigger"
              type="button"
              :aria-label="t('切换语言')"
              :aria-expanded="localeMenuOpen"
              :title="locale === 'zh-CN' ? 'English' : '简体中文'"
              @click="toggleLocaleMenu"
            >
              <Languages :size="16" />
              <span>{{ locale === 'zh-CN' ? '简体中文' : 'English' }}</span>
            </button>
            <div v-if="localeMenuOpen" class="language-menu">
              <button type="button" :class="{ selected: locale === 'zh-CN' }" @click="setLocale('zh-CN')">
                <span>简体中文</span><Check v-if="locale === 'zh-CN'" :size="15" />
              </button>
              <button type="button" :class="{ selected: locale === 'en-US' }" @click="setLocale('en-US')">
                <span>English</span><Check v-if="locale === 'en-US'" :size="15" />
              </button>
            </div>
          </div>
          <button
            class="theme-toggle icon-button"
            type="button"
            :aria-label="darkMode ? t('切换到浅色模式') : t('切换到暗夜模式')"
            :title="darkMode ? t('浅色模式') : t('暗夜模式')"
            @click="toggleTheme"
          >
            <Sun v-if="!darkMode" :size="16" />
            <Moon v-else :size="16" />
          </button>
          <button class="profile-trigger" type="button" :aria-label="`${t('账户')}：${profile.username || 'admin'}`" @click="setPage('account')">
            <UserRound :size="17" />
            <span class="profile-name">{{ profile.username || 'admin' }}</span>
            <ChevronDown :size="14" />
          </button>
        </div>
      </header>

      <transition name="toast-fade">
        <div v-if="notice" class="toast-notice" role="status" aria-live="polite">
          <span class="toast-icon"><CircleCheck :size="14" /></span>
          <span>{{ notice }}</span>
        </div>
      </transition>

      <div class="content">
        <div v-if="error" class="notice error" role="alert">{{ error }}</div>

        <section v-if="page === 'home'" class="dashboard">
          <section class="panel media-overview">
            <div class="panel-head">
              <div class="panel-title-line"><Database :size="18" /><h2>{{ t('媒体库总览') }}</h2></div>
              <span class="status-dot">{{ t('在线') }} {{ mediaOverviews.length }}</span>
            </div>
            <div v-if="mediaOverviews.length" class="stat-grid four">
              <div class="stat-card">
                <span>{{ t('电影') }}</span>
                <strong>{{ mediaOverviewTotals.movie_count.toLocaleString() }}</strong>
                <small>Movies</small>
              </div>
              <div class="stat-card">
                <span>{{ t('剧集') }}</span>
                <strong>{{ mediaOverviewTotals.series_count.toLocaleString() }}</strong>
                <small>Series</small>
              </div>
              <div class="stat-card">
                <span>{{ t('总集数') }}</span>
                <strong>{{ mediaOverviewTotals.episode_count.toLocaleString() }}</strong>
                <small>Episodes</small>
              </div>
              <div class="stat-card">
                <span>{{ t('用户') }}</span>
                <strong>{{ mediaOverviewTotals.user_count.toLocaleString() }}</strong>
                <small>Users</small>
              </div>
            </div>
            <div v-else class="empty-state">{{ overviewError || t('正在读取媒体库总览') }}</div>
            <div
              v-if="mediaOverviews.length"
              class="overview-server-list"
              :class="{ 'has-multiple-servers': mediaOverviews.length > 1 }"
            >
              <div v-for="overview in mediaOverviews" :key="overview.server_name" class="overview-server-row">
                <strong>{{ overview.server_name }}</strong>
                <span>{{ t('电影') }} {{ overview.movie_count.toLocaleString() }}</span>
                <span>{{ t('剧集') }} {{ overview.series_count.toLocaleString() }}</span>
                <span>{{ t('集数') }} {{ overview.episode_count.toLocaleString() }}</span>
                <span>{{ t('用户') }} {{ overview.user_count.toLocaleString() }}</span>
                <small>Emby {{ overview.version }} · {{ overview.operating_system }} · {{ overview.library_count }} {{ t('个媒体库') }}</small>
              </div>
            </div>
          </section>

          <section class="panel health-panel">
            <div class="panel-head">
              <div>
                <div class="panel-title-line"><Gauge :size="18" /><h2>{{ t('服务器状态') }}</h2></div>
                <small class="health-subtitle">{{ t('运行') }} {{ formatUptime(serverHealth?.uptime_seconds) }}</small>
              </div>
            </div>
            <div v-if="serverHealth" class="health-lines">
              <div class="health-line">
                <div>
                  <strong>CPU</strong>
                  <span>{{ serverHealth.cpu_name }} · {{ serverHealth.cpu_cores }} {{ t('核') }}</span>
                </div>
                <b>{{ serverHealth.cpu_percent }}%</b>
                <div class="health-track">
                  <div class="health-bar cpu" :style="{ width: `${serverHealth.cpu_percent}%` }" />
                </div>
              </div>
              <div class="health-line">
                <div>
                    <strong>{{ t('内存') }}</strong>
                  <span>{{ formatBytes(serverHealth.memory_used_bytes) }} / {{ formatBytes(serverHealth.memory_total_bytes) }}</span>
                </div>
                <b>{{ serverHealth.memory_percent }}%</b>
                <div class="health-track">
                  <div class="health-bar memory" :style="{ width: `${serverHealth.memory_percent}%` }" />
                </div>
              </div>
              <div class="health-line">
                <div>
                    <strong>{{ t('磁盘') }}</strong>
                  <span>{{ formatBytes(serverHealth.disk_used_bytes) }} / {{ formatBytes(serverHealth.disk_total_bytes) }}</span>
                </div>
                <b>{{ serverHealth.disk_percent }}%</b>
                <div class="health-track">
                  <div class="health-bar disk" :style="{ width: `${serverHealth.disk_percent}%` }" />
                </div>
              </div>
            </div>
            <div v-else class="empty-state">{{ healthError || t('正在读取服务器状态') }}</div>
          </section>

          <section class="panel operations-panel">
            <div class="panel-head">
              <div>
                <div class="panel-title-line"><Activity :size="18" /><h2>{{ t('运维概览') }}</h2></div>
                <p class="muted">{{ t('健康检查、反代监听和今日请求统计。') }}</p>
              </div>
              <button class="secondary" @click="refreshOperationalData"><RefreshCw :size="15" />{{ t('刷新') }}</button>
            </div>
            <div class="stat-grid four">
              <div class="stat-card">
                <span>{{ t('今日请求') }}</span>
                <strong>{{ requestStatsTotals.requests.toLocaleString() }}</strong>
                <small>{{ detailedHealth?.status === 'ok' ? t('健康') : t('待检查') }}</small>
              </div>
              <div class="stat-card">
                <span>{{ t('重定向') }}</span>
                <strong>{{ requestStatsTotals.redirects.toLocaleString() }}</strong>
                <small>{{ t('STRM 直链') }}</small>
              </div>
              <div class="stat-card">
                <span>{{ t('缓存命中') }}</span>
                <strong>{{ requestStatsTotals.cache_hits.toLocaleString() }}</strong>
                <small>{{ t('内存直链缓存') }}</small>
              </div>
              <div class="stat-card">
                <span>{{ t('拦截 / 错误') }}</span>
                <strong>{{ requestStatsTotals.blocks.toLocaleString() }} / {{ requestStatsTotals.errors.toLocaleString() }}</strong>
                <small>{{ t('今日累计') }}</small>
              </div>
            </div>
            <div class="connectivity-list">
              <div
                v-for="row in operationalServerRows"
                :key="`operation-${row.server.id}`"
                class="connectivity-row"
              >
                <div>
                  <strong>{{ row.server.name }}</strong>
                  <small>
                    {{ t('端口') }} :{{ row.proxy?.port || row.server.port }} ·
                    {{ t('最近请求') }} {{ formatTimestampMs(row.proxy?.last_request_ms) }} ·
                    {{ t('最近巡检') }} {{ formatTimestampMs(row.connectivity?.checked_at_ms || 0) }}
                    <template v-if="row.connectivity"> · {{ t('耗时') }} {{ row.connectivity.duration_ms }}ms</template>
                  </small>
                </div>
                <span :class="['client-badge', proxyStatusClass(row.proxy)]">
                  {{ proxyStatusLabel(row.proxy) }}
                </span>
                <span :class="['client-badge', row.connectivity?.ok ? 'allowed' : 'blocked']">
                  {{ row.connectivity ? connectivityStatusLabel(row.connectivity) : t('未巡检') }}
                </span>
                <span>Emby {{ healthPartLabel(row.connectivity?.emby_ok ?? null) }}</span>
                <span>OpenList {{ healthPartLabel(row.connectivity?.openlist_ok ?? null) }}</span>
                <span>{{ t('反代') }} {{ healthPartLabel(row.connectivity?.proxy_ok ?? null) }}</span>
                  <span>{{ row.connectivity ? failedDuration(row.connectivity) : t('无失败') }}</span>
                <small v-if="row.connectivity?.auto_restarted_at_ms">
                  {{ t('最近自动重启') }} {{ formatTimestampMs(row.connectivity.auto_restarted_at_ms) }}
                </small>
                <small v-if="row.connectivity?.last_error" class="server-status-error">{{ row.connectivity.last_error }}</small>
              </div>
            </div>
          </section>

          <section class="panel rate-limit-overview">
            <div class="panel-head">
              <div>
                <div class="panel-title-line"><ShieldCheck :size="18" /><h2>{{ t('播放频率限制') }}</h2></div>
                <p class="muted">{{ t('首页直接查看当前窗口命中和封禁情况。') }}</p>
              </div>
              <button class="secondary" @click="refreshRateLimitStatus"><RefreshCw :size="15" />{{ t('刷新') }}</button>
            </div>
            <div class="stat-grid three">
              <div class="stat-card">
                <span>{{ t('活跃窗口') }}</span>
                <strong>{{ rateLimitOverview.active_windows.toLocaleString() }}</strong>
                <small>{{ t('当前监控中的 IP') }}</small>
              </div>
              <div class="stat-card">
                <span>{{ t('已封禁') }}</span>
                <strong>{{ rateLimitOverview.blocked_windows.toLocaleString() }}</strong>
                <small>{{ t('屏蔽 IP / 禁用用户') }}</small>
              </div>
              <div class="stat-card">
                <span>{{ t('最高命中') }}</span>
                <strong>{{ rateLimitOverview.highest_count.toLocaleString() }}</strong>
                <small>{{ t('当前窗口最大次数') }}</small>
              </div>
            </div>
            <div v-if="rateLimitWindows.length" class="rate-block-table-wrap home-rate-table">
              <table class="rate-block-table">
                <thead>
                  <tr>
                    <th>{{ t('封禁方式') }}</th>
                    <th>{{ t('服务器') }}</th>
                    <th>IP</th>
                    <th>{{ t('用户') }}</th>
                    <th>{{ t('命中') }}</th>
                    <th>{{ t('窗口') }}</th>
                    <th>{{ t('状态') }}</th>
                    <th>{{ rateLimitWindows.length }} {{ t('条') }}</th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="row in rateLimitWindows.slice(0, 5)" :key="`home-${row.server_id}-${row.ip}`">
                    <td>{{ playbackRateActionLabel(row.block_action) }}</td>
                    <td>{{ formatServerName(row.server_id) }}</td>
                    <td>
                      <strong>{{ row.ip }}</strong>
                      <small v-if="formatIpLocation(row.ip_location)" class="ip-location">{{ formatIpLocation(row.ip_location) }}</small>
                    </td>
                    <td>{{ row.user_name || '--' }}</td>
                    <td>{{ row.current_count }}/{{ row.threshold }}</td>
                    <td>{{ row.window_seconds }}s</td>
                    <td>
                      <span :class="['client-badge', row.blocked ? 'blocked' : 'allowed']">
                        {{ row.blocked ? t('已封禁') : t('观察中') }}
                      </span>
                    </td>
                    <td>
                      <button
                        v-if="row.blocked && row.block_id"
                        type="button"
                        class="secondary"
                        @click="unblockRateLimitWindow(row)"
                      >
                        {{ t('解除封禁') }}
                      </button>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
            <div v-else class="empty-state compact">{{ t('当前没有播放频率窗口数据。') }}</div>
          </section>

          <section class="panel playing-panel">
            <div class="panel-head">
              <div class="panel-title-line"><PlayCircle :size="18" /><h2>{{ t('实时播放') }}</h2></div>
              <div class="panel-actions">
                <span class="status-dot">{{ t('在线') }} {{ activePlayCount }}</span>
                <button class="secondary" :disabled="playbackLoading" @click="refreshPlaybackSessions">
                  <RefreshCw :size="15" />{{ playbackLoading ? t('刷新中') : t('刷新') }}
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
                  <div v-if="session.playback_ip" class="play-ip">
                    IP {{ ipWithLocation(session.playback_ip, session.ip_location) }}
                  </div>
                  <div class="progress-track">
                    <div class="progress-bar" :style="{ width: `${session.percent ?? 0}%` }" />
                  </div>
                  <div class="play-footer">
                    <span>{{ formatTicks(session.position_ticks) }} / {{ formatTicks(session.runtime_ticks) }}</span>
                    <span>{{ session.transcoding ? t('转码') : session.play_method || t('直放') }}</span>
                  </div>
                </div>
              </article>
            </div>
            <div v-else class="empty-state">
              {{ playbackLoading ? t('正在读取 Emby 播放会话') : playbackError || t('当前没有正在播放的媒体') }}
            </div>
          </section>
        </section>

        <section v-else-if="page === 'server'" class="panel">
          <div class="panel-head">
            <div>
              <div class="panel-title-line"><Server :size="18" /><h2>{{ t('服务器配置') }}</h2></div>
              <p class="muted">{{ t('每个 Emby 服务器使用独立反代端口，保存后自动监听。') }}</p>
            </div>
            <div class="panel-actions">
              <button class="secondary" @click="addServer"><Plus :size="15" />{{ t('添加服务器') }}</button>
              <button class="secondary" :disabled="saving" @click="validateSettings"><ShieldCheck :size="15" />{{ t('测试配置') }}</button>
              <button class="primary" :disabled="saving" @click="saveSettings">
                <Check :size="15" />{{ saving ? t('保存中') : t('保存配置') }}
              </button>
            </div>
          </div>
          <div class="server-list">
            <article v-for="(server, index) in settings.servers" :key="server.id" class="server-card">
              <div class="server-card-head">
                <strong>{{ server.name || `${t('服务器')} ${index + 1}` }}</strong>
                <div class="server-actions">
                  <button
                    type="button"
                    class="secondary"
                    :aria-expanded="isServerExpanded(server.id)"
                    @click="toggleServerExpanded(server.id)"
                  >
                    <ChevronDown v-if="!isServerExpanded(server.id)" :size="15" />
                    <ChevronRight v-else :size="15" />
                    {{ isServerExpanded(server.id) ? t('收起配置') : t('展开配置') }}
                  </button>
                  <button
                    type="button"
                    :class="['server-toggle-button', { disabled: !server.enabled }]"
                    :disabled="saving || restartingServerId === server.id"
                    :aria-pressed="server.enabled"
                    @click="toggleProxyServer(server)"
                  >
                    <Zap :size="15" />{{ server.enabled ? t('关闭服务器') : t('开启服务器') }}
                  </button>
                  <button
                    type="button"
                    class="secondary restart-button"
                    :disabled="saving || restartingServerId === server.id || !server.enabled"
                    @click="restartProxyServer(server)"
                  >
                    <RotateCw :size="15" />{{ restartingServerId === server.id ? t('重启中') : t('重启服务器') }}
                  </button>
                  <button class="danger-button" @click="removeServer(server.id)">
                    <Trash2 :size="15" />{{ t('删除') }}
                  </button>
                </div>
              </div>
              <div class="server-status-strip">
                <span :class="['client-badge', proxyStatusById[server.id]?.listening ? 'allowed' : 'blocked']">
                  {{ proxyStatusLabel(proxyStatusById[server.id]) }}
                </span>
                <span>{{ t('端口') }} :{{ proxyStatusById[server.id]?.port || server.port }}</span>
                <span>{{ t('启动') }} {{ formatTimestampMs(proxyStatusById[server.id]?.started_at_ms) }}</span>
                <span>{{ t('最近请求') }} {{ formatTimestampMs(proxyStatusById[server.id]?.last_request_ms) }}</span>
                <span v-if="proxyStatusById[server.id]?.last_error" class="server-status-error">
                  {{ proxyStatusById[server.id]?.last_error }}
                </span>
              </div>
              <div v-if="isServerExpanded(server.id)" class="server-config-body">
                <div class="grid server-grid">
                  <label>
                    <span>{{ t('名称') }}</span>
                    <input v-model="server.name" :placeholder="t('例如：主服务器')" />
                  </label>
                  <label>
                    <span>{{ t('Emby 地址') }}</span>
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
                        :aria-label="isApiKeyVisible(server.id) ? t('隐藏 Emby API Key') : t('显示 Emby API Key')"
                        :title="isApiKeyVisible(server.id) ? t('隐藏 Emby API Key') : t('显示 Emby API Key')"
                        @click="toggleApiKeyVisible(server.id)"
                      >
                        <EyeOff v-if="!isApiKeyVisible(server.id)" :size="16" aria-hidden="true" />
                        <Eye v-else :size="16" aria-hidden="true" />
                      </button>
                    </div>
                  </label>
                  <label>
                    <span>{{ t('反代端口') }}</span>
                    <input v-model.number="server.port" type="number" min="1" max="65535" />
                  </label>
                </div>
                <div class="grid real-ip-grid">
                  <label>
                    <span>{{ t('真实 IP 获取方式') }}</span>
                    <select v-model="server.real_ip_mode" @change="updateRealIpMode(server)">
                      <option v-for="option in realIpModeOptions" :key="option.value" :value="option.value">
                        {{ t(option.label) }}
                      </option>
                    </select>
                    <small v-if="server.real_ip_mode === 'header_list'" class="field-help">
                      {{ t('从下列常用 CDN 携带真实 IP 的 HTTP Header 中获取，按顺序取第一个能获取到的值。') }}
                    </small>
                  </label>
                  <label v-if="needsRealIpHeader(server)">
                    <span>{{ server.real_ip_mode === 'header' ? 'HTTP Header' : 'CDN Headers' }}</span>
                    <textarea
                      v-model="server.real_ip_header"
                      :placeholder="
                        server.real_ip_mode === 'header'
                          ? t('例如：x-real-ip')
                          : defaultCdnHeaders
                      "
                    />
                  </label>
                </div>
                <p class="muted real-ip-help">
                  {{ t('默认使用系统识别。经过 CDN 或多层反代后 IP 不准时再配置，保存后会同步重启对应反代服务。') }}
                </p>
                <div class="server-config-actions">
                  <button class="primary" :disabled="saving" @click="saveSettings">
                    <Check :size="15" />{{ saving ? t('保存中') : t('保存配置') }}
                  </button>
                </div>
              </div>
            </article>
          </div>

          <div class="grid common-grid">
            <label>
              <span>{{ t('缓存秒数') }}</span>
              <input class="compact-number-input" v-model.number="settings.cache_ttl_seconds" type="number" min="0" />
            </label>
            <label>
              <span>{{ t('缓存最大条数') }}</span>
              <input v-model.number="settings.cache_max_capacity" type="number" min="1" />
            </label>
            <label>
              <span>{{ t('OpenList 地址') }}</span>
              <input v-model="settings.openlist_addr" :placeholder="`${t('可选')}：http://openlist.local:5244`" />
            </label>
            <label>
              <span>OpenList Token</span>
              <input v-model="settings.openlist_token" type="password" :placeholder="t('可选')" />
            </label>
          </div>
          <div class="grid health-check-grid">
            <label class="check setting-check">
              <input v-model="settings.connectivity_check_enabled" type="checkbox" />
              <span>{{ t('启用服务器连通性巡检') }}</span>
            </label>
            <label>
              <span>{{ t('巡检间隔秒数') }}</span>
              <input class="compact-number-input" v-model.number="settings.connectivity_check_interval_seconds" type="number" min="10" max="3600" />
            </label>
            <label>
              <span>{{ t('单项超时秒数') }}</span>
              <input class="compact-number-input" v-model.number="settings.connectivity_check_timeout_seconds" type="number" min="1" max="60" />
            </label>
            <label>
              <span>{{ t('反代无响应自动重启秒数') }}</span>
              <input class="compact-number-input" v-model.number="settings.connectivity_auto_restart_seconds" type="number" min="0" max="86400" />
              <small class="field-help">{{ t('填 0 表示不自动重启；只在反代端口连续无响应时触发。') }}</small>
            </label>
          </div>
          <div class="advanced-routing-grid">
            <label class="cache-filter-mode">
              <span>{{ t('缓存过滤模式') }}</span>
              <select v-model="settings.cache_domain_filter_mode">
                <option value="off">{{ t('不过滤') }}</option>
                <option value="whitelist">{{ t('白名单：命中才缓存') }}</option>
                <option value="blacklist">{{ t('黑名单：命中不缓存') }}</option>
              </select>
            </label>
            <label class="cache-filter-domains">
              <span>{{ t('缓存过滤域名') }}</span>
              <textarea
                v-model="settings.cache_domain_whitelist"
                :disabled="settings.cache_domain_filter_mode === 'off'"
                :placeholder="t('支持多个域名、通配符或关键字；每行一个，例如：*.115cdn.* 或 115')"
              />
              <small class="field-help">
                {{ t('只匹配直链域名部分。白名单命中才缓存；黑名单命中不缓存，其他直链正常缓存。') }}
              </small>
            </label>
            <div class="head-resolve-grid">
              <label class="check">
                <input v-model="settings.enable_internal_redirect" type="checkbox" />
                <span>{{ t('开启内部重定向 HEAD 解析') }}</span>
              </label>
              <label>
                <span>{{ t('HEAD 超时秒数') }}</span>
                <input class="compact-number-input" v-model.number="settings.internal_redirect_timeout_seconds" type="number" min="1" />
              </label>
            </div>
            <label class="strm-mapping-field">
              <span>{{ t('STRM URL 映射') }}</span>
              <textarea
                class="strm-mapping-input"
                v-model="settings.strm_url_mappings"
                spellcheck="false"
                :placeholder="strmMappingPlaceholder"
              />
            </label>
          </div>

          <section class="config-tools single">
            <div class="tool-block">
              <div class="panel-head compact">
                <h3>{{ t('配置测试结果') }}</h3>
                <button class="secondary" :disabled="saving" @click="validateSettings"><RefreshCw :size="15" />{{ t('重新测试') }}</button>
              </div>
              <div v-if="validationResults.length" class="validation-list">
                <div
                  v-for="result in validationResults"
                  :key="`${result.scope}-${result.message}-${result.detail}`"
                  :class="['validation-row', validationClass(result)]"
                >
                  <strong>{{ localizeValidationText(result.scope) }}</strong>
                  <span>{{ localizeValidationText(result.message) }}</span>
                  <small>{{ result.detail ? localizeValidationText(result.detail) : '--' }}</small>
                </div>
              </div>
              <div v-else class="empty-state compact">{{ t('还没有运行配置测试。') }}</div>
            </div>
          </section>
        </section>

        <section v-else-if="page === 'clients'" class="client-page">
          <section class="panel">
            <div class="panel-head">
              <div>
                <div class="panel-title-line"><Users :size="18" /><h2>{{ t('客户端管控') }}</h2></div>
                <p class="muted">{{ t('自动记录播放设备和 UA，也可以按播放频率临时禁用账号。') }}</p>
              </div>
              <div class="panel-actions">
                <button class="secondary" @click="refreshClientControl"><RefreshCw :size="15" />{{ t('刷新') }}</button>
                <button class="primary" :disabled="savingClientControl" @click="saveClientControl">
                  <Check :size="15" />{{ savingClientControl ? t('保存中') : t('保存') }}
                </button>
              </div>
            </div>
            <div v-if="clientControlError" class="notice error" role="alert">{{ clientControlError }}</div>
            <div class="client-toolbar">
              <label class="check">
                <input v-model="clientControl.enabled" type="checkbox" />
                <span>{{ t('启用 UA 拦截') }}</span>
              </label>
              <label class="check">
                <input v-model="clientControl.playback_rate_limit_enabled" type="checkbox" />
                <span>{{ t('启用播放频率限制') }}</span>
              </label>
              <label class="check">
                <input v-model="clientControl.concurrent_playback_limit_enabled" type="checkbox" />
                <span>{{ t('启用同时播放限制') }}</span>
              </label>
            </div>
            <div class="rate-limit-grid">
              <label>
                <span>{{ t('屏蔽方式') }}</span>
                <select v-model="clientControl.playback_rate_limit_action">
                  <option v-for="option in playbackLimitActionOptions" :key="option.value" :value="option.value">
                    {{ t(option.label) }}
                  </option>
                </select>
              </label>
              <label>
                <span>{{ t('检测时间窗口（秒）') }}</span>
                <input v-model.number="clientControl.playback_rate_limit_window_seconds" type="number" min="1" />
              </label>
              <label>
                <span>{{ t('最大播放次数') }}</span>
                <input v-model.number="clientControl.playback_rate_limit_max_requests" type="number" min="1" />
              </label>
              <label>
                <span>{{ t('封禁时长（秒）') }}</span>
                <input v-model.number="clientControl.playback_rate_limit_block_seconds" type="number" min="1" />
              </label>
              <label>
                <span>{{ t('允许同时播放数') }}</span>
                <input
                  v-model.number="clientControl.concurrent_playback_limit_max"
                  type="number"
                  min="1"
                  :disabled="!clientControl.concurrent_playback_limit_enabled"
                />
              </label>
            </div>
            <div class="rate-block-list">
              <div v-if="activeRateLimitBlocks.length" class="rate-block-table-wrap">
                <table class="rate-block-table client-rate-block-table">
                  <thead>
                    <tr>
                      <th>{{ t('封禁方式') }}</th>
                      <th>{{ t('封禁原因') }}</th>
                      <th>{{ t('服务器') }}</th>
                      <th>IP</th>
                      <th>{{ t('用户') }}</th>
                      <th>{{ t('到期时间') }}</th>
                      <th>{{ activeRateLimitBlocks.length }} {{ t('条') }}</th>
                    </tr>
                  </thead>
                  <tbody>
                    <tr v-for="record in activeRateLimitBlocks" :key="record.id">
                      <td>{{ playbackRateActionLabel(record.action) }}</td>
                      <td>{{ rateLimitBlockReason(record) }}</td>
                      <td>{{ record.server_name }}</td>
                      <td>
                        <strong>{{ rateLimitBlockIp(record) }}</strong>
                        <small v-if="formatIpLocation(record.ip_location)" class="ip-location">{{ formatIpLocation(record.ip_location) }}</small>
                      </td>
                      <td>{{ record.user_name || '--' }}</td>
                      <td>{{ formatTimestamp(record.blocked_until) }}</td>
                      <td>
                        <button class="secondary" @click="unblockRateLimit(record)"><ShieldCheck :size="15" />{{ t('解除封禁') }}</button>
                      </td>
                    </tr>
                  </tbody>
                </table>
              </div>
              <div v-else class="empty-state compact">{{ t('暂无频率限制封禁。') }}</div>
            </div>
          </section>

          <section class="panel">
            <div class="panel-head">
              <div>
                <div class="panel-title-line"><Clock3 :size="18" /><h2>{{ t('播放频率窗口') }}</h2></div>
                <p class="muted">{{ t('显示当前检测窗口内各 IP 的播放请求计数。') }}</p>
              </div>
              <button class="secondary" @click="refreshRateLimitStatus"><RefreshCw :size="15" />{{ t('刷新') }}</button>
            </div>
            <div v-if="rateLimitWindows.length" class="rate-window-table-wrap">
              <table class="rate-window-table">
                <thead>
                  <tr>
                    <th>{{ t('服务器') }}</th>
                    <th>IP</th>
                    <th>{{ t('用户') }}</th>
                    <th>{{ t('当前次数') }}</th>
                    <th>{{ t('阈值') }}</th>
                    <th>{{ t('剩余') }}</th>
                    <th>{{ t('窗口') }}</th>
                    <th>{{ t('重置时间') }}</th>
                    <th>{{ t('状态') }}</th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="row in rateLimitWindows" :key="`${row.server_id}-${row.ip}`">
                    <td>{{ formatServerName(row.server_id) }}</td>
                    <td>
                      <strong>{{ row.ip }}</strong>
                      <small v-if="formatIpLocation(row.ip_location)" class="ip-location">{{ formatIpLocation(row.ip_location) }}</small>
                    </td>
                    <td>{{ row.user_name || '--' }}</td>
                    <td>{{ row.current_count }}</td>
                    <td>{{ row.threshold }}</td>
                    <td>{{ row.remaining }}</td>
                    <td>{{ row.window_seconds }}s</td>
                    <td>{{ formatTimestamp(row.reset_at) }}</td>
                    <td>
                      <span :class="['client-badge', row.blocked ? 'blocked' : 'allowed']">
                        {{ row.blocked ? t('已封禁') : t('观察中') }}
                      </span>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
            <div v-else class="empty-state compact">{{ t('当前没有播放频率窗口数据。') }}</div>
          </section>

          <div class="client-filterbar client-filterbar-standalone">
            <button
              :class="['filter-button', { active: clientStatusFilter === 'all' }]"
              @click="clientStatusFilter = 'all'"
            >
              {{ t('全部') }} {{ clientControl.records.length }}
            </button>
            <button
              :class="['filter-button', { active: clientStatusFilter === 'blocked' }]"
              @click="clientStatusFilter = 'blocked'"
            >
              {{ t('已禁用') }} {{ blockedClientCount }}
            </button>
            <button
              :class="['filter-button', { active: clientStatusFilter === 'allowed' }]"
              @click="clientStatusFilter = 'allowed'"
            >
              {{ t('允许播放') }} {{ allowedClientCount }}
            </button>
            <input
              v-model="clientKeywordFilter"
              class="client-search"
              :placeholder="t('搜索 UA')"
            />
            <button
              class="secondary"
              :disabled="clientStatusFilter === 'all' && !clientKeywordFilter"
              @click="clearClientFilters"
            >
              {{ t('清空筛选') }}
            </button>
          </div>

          <section class="panel">
            <div class="panel-head">
              <div>
                <div class="panel-title-line"><Plus :size="18" /><h2>{{ t('手动添加 UA 拦截') }}</h2></div>
                <p class="muted">{{ t('输入 UA 完整内容或关键字，保存后默认进入禁用状态。') }}</p>
              </div>
            </div>
            <div class="manual-rule-row">
              <label>
                <span>{{ t('UA 关键字') }}</span>
                <input
                  v-model="manualClientRule.user_agent"
                  :placeholder="t('例如：Infuse / Fileball / okhttp')"
                  @keyup.enter="addClientRule"
                />
              </label>
              <label>
                <span>{{ t('描述') }}</span>
                <input v-model="manualClientRule.note" :placeholder="t('例如：临时禁用某客户端')" @keyup.enter="addClientRule" />
              </label>
              <button class="primary" :disabled="addingClientRule" @click="addClientRule">
                <Plus :size="15" />{{ addingClientRule ? t('添加中') : t('添加拦截') }}
              </button>
            </div>
          </section>

          <section class="panel client-table-panel">
            <div class="client-date-note">
              <strong>{{ t('日期说明') }}</strong>
              <span>{{ t('时间为该 UA 第一次出现或手动添加的时间；后台更新时间只在规则状态、备注或客户端信息变化时刷新，同一 UA 重复请求不会每次刷新。') }}</span>
            </div>
            <div class="client-table-wrap">
              <table class="client-table">
                <thead>
                  <tr>
                    <th>{{ t('关键字') }}</th>
                    <th>{{ t('描述') }}</th>
                    <th>{{ t('状态') }}</th>
                    <th>{{ t('记录时间') }}</th>
                    <th>{{ t('操作') }}</th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="record in clientRuleRows" :key="record.id">
                    <td>
                      <strong :title="clientKeyword(record)">{{ clientKeyword(record) }}</strong>
                    </td>
                    <td>
                      <span :title="record.note || (record.source === 'auto' ? t('自动记录播放设备') : t('手动 UA 拦截'))">
                        {{ record.note || (record.source === 'auto' ? t('自动记录播放设备') : t('手动 UA 拦截')) }}
                      </span>
                    </td>
                    <td>
                      <span :class="['client-badge', record.enabled ? 'blocked' : 'allowed']">
                        {{ record.enabled ? t('已禁用') : t('允许播放') }}
                      </span>
                    </td>
                    <td class="client-time-cell">
                      {{ formatTimestamp(record.created_at) }}
                    </td>
                    <td>
                      <div class="rule-actions">
                        <button
                          type="button"
                          :class="['switch-button', { active: record.enabled }]"
                          :aria-pressed="record.enabled"
                          :aria-label="record.enabled ? t('关闭 UA 拦截') : t('开启 UA 拦截')"
                          @click="toggleClientRule(record)"
                        >
                          <span />
                        </button>
                        <button type="button" class="danger-button" @click="deleteClientRule(record)">
                          <Trash2 :size="15" />{{ t('删除') }}
                        </button>
                      </div>
                    </td>
                  </tr>
                </tbody>
              </table>
              <div v-if="!clientRuleRows.length" class="empty-state">
                {{ clientControl.records.length ? t('当前筛选没有匹配的客户端。') : t('暂无客户端记录，开始播放后会自动出现，也可以手动添加 UA 拦截。') }}
              </div>
            </div>
          </section>
        </section>

        <section v-else-if="page === 'notifications'" class="client-page">
          <section class="panel webhook-panel">
            <div class="panel-head">
              <div>
                <div class="panel-title-line"><Bell :size="18" /><h2>{{ t('通知配置') }}</h2></div>
                <p class="muted">{{ t('Webhook 使用 POST JSON 发送：{ title, text }。命中通知包含播放频率屏蔽和 UA 拦截命中。') }}</p>
              </div>
              <div class="panel-actions">
                <label class="check compact-check">
                  <input v-model="clientControl.notify_enabled" type="checkbox" />
                  <span>{{ t('命中通知') }}</span>
                </label>
                <button class="secondary" @click="addWebhook"><Plus :size="15" />{{ t('添加 Webhook') }}</button>
                <button class="primary" :disabled="savingClientControl" @click="saveClientControl">
                  <Check :size="15" />{{ savingClientControl ? t('保存中') : t('保存通知') }}
                </button>
              </div>
            </div>
            <div v-if="clientControlError" class="notice error" role="alert">{{ clientControlError }}</div>
            <div class="webhook-list">
              <article v-for="(webhook, index) in clientControl.webhooks" :key="webhook.id" class="webhook-item">
                <div class="webhook-item-head">
                  <label class="check compact-check">
                    <input v-model="webhook.enabled" type="checkbox" />
                    <span>{{ t('启用') }}</span>
                  </label>
                  <div class="rule-actions">
                    <button class="secondary" :disabled="testingWebhook" @click="testWebhook(webhook)">
                      <Webhook :size="15" />{{ testingWebhook ? t('测试中') : t('测试连接') }}
                    </button>
                    <button class="danger-button" @click="removeWebhook(index)"><Trash2 :size="15" />{{ t('删除') }}</button>
                  </div>
                </div>
                <div class="grid webhook-grid">
                  <label>
                    <span>{{ t('名称') }}</span>
                    <input v-model="webhook.name" :placeholder="t('例如：企业微信通知')" />
                  </label>
                  <label>
                    <span>Webhook URL</span>
                    <input v-model="webhook.url" placeholder="https://example.com/webhook" />
                  </label>
                  <label>
                    <span>{{ t('密钥（可选）') }}</span>
                    <input v-model="webhook.secret" type="password" :placeholder="t('可选密钥')" />
                  </label>
                </div>
              </article>
            </div>
            <p class="muted rate-limit-help">{{ t('请求体固定为 {"title":"${title}","text":"${text}"}，密钥会通过 `X-Webhook-Secret` 头发送。') }}</p>
          </section>
        </section>

        <section v-else-if="page === 'backup'" class="backup-page">
          <section class="panel">
            <div class="panel-head">
              <div>
                <div class="panel-title-line"><Archive :size="18" /><h2>{{ t('配置备份') }}</h2></div>
                <p class="muted">{{ t('导出配置文件，或从电脑选择配置文件还原运行配置。') }}</p>
              </div>
            </div>
            <input
              ref="backupFileInput"
              class="visually-hidden"
              type="file"
              accept=".json,application/json,text/plain"
              @change="handleBackupFileSelected"
            />
            <div v-if="backupError" class="notice error" role="alert">{{ backupError }}</div>
            <div class="backup-layout">
              <section class="backup-card">
                <h3>{{ t('备份范围') }}</h3>
                <div class="backup-scope-grid">
                  <div>
                    <strong>{{ t('服务器配置') }}</strong>
                    <span>{{ t('Emby 地址、API Key、反代端口、真实 IP、缓存和映射规则') }}</span>
                  </div>
                  <div>
                    <strong>{{ t('客户端管控') }}</strong>
                    <span>{{ t('UA 拦截、播放频率限制、封禁列表和客户端规则') }}</span>
                  </div>
                  <div>
                    <strong>{{ t('通知配置') }}</strong>
                    <span>{{ t('Webhook 地址、启用状态和密钥') }}</span>
                  </div>
                  <div>
                    <strong>{{ t('日志配置') }}</strong>
                    <span>{{ t('日志级别、文件大小、保留数量和格式') }}</span>
                  </div>
                </div>
                <p class="muted backup-note">
                  {{ t('备份文件会使用备份密码加密；不包含面板管理员用户名、密码、登录会话、运行日志文件和请求统计数据。') }}
                </p>
              </section>

              <section class="backup-card">
                <h3>{{ t('配置文件备份 / 还原') }}</h3>
                <div class="backup-actions text-actions">
                  <button class="secondary" @click="exportBackup"><Download :size="15" />{{ t('导出备份') }}</button>
                  <button class="primary" @click="importBackup"><Upload :size="15" />{{ t('还原') }}</button>
                </div>
                <div class="backup-drop-hint">
                  <strong>{{ t('导出备份') }}</strong>
                  <span>{{ t('输入备份密码后生成加密的 `embypanel-config-时间.json` 并弹出浏览器下载。') }}</span>
                  <strong>{{ t('还原') }}</strong>
                  <span>{{ t('点击后选择本机配置文件，加密备份需要输入对应密码，读取成功后自动还原并重启反代服务。') }}</span>
                </div>
              </section>
            </div>
          </section>
        </section>

        <section v-else-if="page === 'logs'" class="logs-page">
          <section class="panel log-console-panel">
            <div class="panel-head">
              <div>
                <div class="panel-title-line"><FileText :size="18" /><h2>{{ t('日志') }}</h2></div>
                <p class="muted">
                  {{ t('单列表查看播放日志、拦截日志、反代请求和运行日志，页面打开时每 3 秒自动刷新。') }}
                </p>
              </div>
              <div class="panel-actions">
                <span class="status-dot">{{ logsLoading ? t('刷新中') : t('实时刷新') }}</span>
                <button class="secondary" :disabled="logsLoading" @click="refreshActivityLogs">
                  <RefreshCw :size="15" />{{ logsLoading ? t('刷新中') : t('刷新') }}
                </button>
                <button v-if="!isRequestDetailLogView" class="secondary" @click="exportLogs"><Download :size="15" />{{ t('导出 CSV') }}</button>
              </div>
            </div>
            <div v-if="logsError" class="notice error" role="alert">{{ logsError }}</div>
            <div :class="['log-toolbar', 'compact', { proxy: isRequestDetailLogView }]">
              <label>
                <span>{{ t('日志类型') }}</span>
                <select v-model="selectedLogView" @change="handleLogViewChange">
                  <option value="playback">{{ t('播放日志') }}</option>
                  <option value="blocked">{{ t('拦截日志') }}</option>
                  <option value="proxy">{{ t('反代请求') }}</option>
                  <option value="general">{{ t('运行日志') }}</option>
                </select>
              </label>
              <label v-if="showLogLevelFilter">
                <span>{{ t('级别') }}</span>
                <select v-model="selectedLogLevel" @change="refreshLogsWithReset">
                  <option v-for="option in logLevelOptions" :key="option.value" :value="option.value">
                    {{ t(option.label) }}
                  </option>
                </select>
              </label>
              <label>
                <span>{{ t('服务器') }}</span>
                <select v-model="selectedLogServer" @change="refreshLogsWithReset">
                  <option value="all">{{ t('全部服务器') }}</option>
                  <option v-for="server in logServers" :key="server.id" :value="server.id">
                    {{ server.name }} · :{{ server.port }}
                  </option>
                </select>
              </label>
              <label v-if="isRequestDetailLogView">
                <span>{{ t('请求类型') }}</span>
                <select v-model="selectedRequestPathType" @change="refreshLogsWithReset">
                  <option value="all">{{ t('全部请求') }}</option>
                  <option value="video_stream">{{ t('视频流') }}</option>
                  <option value="playback_info">{{ t('播放信息') }}</option>
                  <option value="system_info">{{ t('系统信息') }}</option>
                  <option value="base_html_player">{{ t('播放器脚本') }}</option>
                  <option v-if="selectedLogView === 'blocked'" value="rate_limit_action">{{ t('封禁动作') }}</option>
                  <option value="proxy">{{ t('普通代理') }}</option>
                </select>
              </label>
              <label class="log-search-field">
                <span>{{ t('关键词') }}</span>
                <input v-model="logKeywordFilter" :placeholder="t('搜索用户 / IP / URL / 信息')" @keyup.enter="refreshLogsWithReset" />
              </label>
              <label>
                <span>{{ t('开始时间') }}</span>
                <input v-model="logSince" type="datetime-local" @change="refreshLogsWithReset" />
              </label>
              <label>
                <span>{{ t('结束时间') }}</span>
                <input v-model="logUntil" type="datetime-local" @change="refreshLogsWithReset" />
              </label>
              <div class="log-filter-actions">
                <button class="primary" @click="refreshLogsWithReset"><SlidersHorizontal :size="15" />{{ t('筛选') }}</button>
              </div>
            </div>

            <div class="log-console-meta">
              <span>{{ selectedLogViewLabel }}</span>
              <strong>{{ visibleLogCount }}</strong>
              <span v-if="isRequestDetailLogView">
                {{ t('单次最多') }} {{ requestDetailDisplayMax }} {{ t('条') }}，{{ t('保留') }} {{ requestDetailPersistDays }} {{ t('天或最近') }} {{ requestDetailPersistMax }} {{ t('条') }}
              </span>
              <span v-else>{{ t('内存最多保留最近') }} {{ activityLogMaxLimit }} {{ t('条可视化日志') }}</span>
            </div>

            <div
              v-if="visibleLogCount"
              class="log-console-list"
              @scroll="handleScrollableLogListScroll($event, loadMoreSelectedLogs)"
            >
              <template v-if="!isRequestDetailLogView">
                <article
                  v-for="entry in visibleActivityLogRows"
                  :key="`activity-${entry.id}`"
                  :class="['log-entry', entry.kind, entry.level]"
                >
                  <div class="log-time">{{ formatLogTime(entry.timestamp_ms) }}</div>
                  <div class="log-body">
                    <div class="log-title">
                      <strong>{{ entry.message }}</strong>
                      <span :class="['level-pill', entry.level]">{{ logLevelLabel(entry.level) }}</span>
                      <span class="server-pill">{{ entry.server_name }}</span>
                      <span v-if="entry.playback_user" class="user-pill">{{ entry.playback_user }}</span>
                      <span v-if="entry.playback_ip" class="ip-pill">{{ ipWithLocation(entry.playback_ip, entry.ip_location) }}</span>
                    </div>
                    <p :class="{ 'log-detail': entry.kind === 'playback' }">
                      <a
                        v-if="entry.detail && isHttpUrl(entry.detail)"
                        class="copy-link"
                        href="#"
                        :title="t('点击复制链接')"
                        @click.prevent="copyText(entry.detail)"
                      >{{ entry.detail }}</a>
                      <template v-else>{{ entry.detail || t('暂无详情') }}</template>
                    </p>
                  </div>
                </article>
              </template>
              <template v-else>
                <article
                  v-for="row in filteredRequestDetailRows"
                  :key="`proxy-${row.id}`"
                  :class="['request-detail-row', requestOutcomeClass(row)]"
                >
                  <div class="request-main">
                    <div class="request-meta">
                      <span :class="['level-pill', requestSeverity(row)]">{{ requestSeverityLabel(row) }}</span>
                      <span class="server-pill">{{ row.server_name }}</span>
                      <span class="user-pill">{{ row.playback_user || '--' }}</span>
                      <span class="ip-pill">{{ ipWithLocation(row.playback_ip, row.ip_location) }}</span>
                      <span class="request-time">{{ formatLogTime(row.timestamp_ms) }}</span>
                    </div>
                    <div class="request-path">
                      <strong>{{ requestRowTitle(row) }}</strong>
                      <span>{{ row.detail }}</span>
                    </div>
                  </div>
                  <div class="request-state">
                    <span>{{ row.outcome }}</span>
                    <span>{{ requestPathTypeLabel(row.path_type) }}</span>
                    <span v-if="row.event_type !== 'block' && row.event_type !== 'unblock'">HTTP {{ row.status_code }}</span>
                    <span v-if="row.event_type !== 'block' && row.event_type !== 'unblock'">{{ row.duration_ms }}ms</span>
                    <span v-if="row.event_type !== 'block' && row.event_type !== 'unblock'">{{ row.cache_hit ? t('缓存命中') : t('未命中') }}</span>
                    <span>{{ row.event_type === 'unblock' ? t('已解除') : row.blocked ? t('已拦截') : t('未拦截') }}</span>
                  </div>
                </article>
              </template>

              <button
                v-if="canLoadMoreSelectedLogs"
                class="load-more-row"
                :disabled="logsLoading"
                @click="loadMoreSelectedLogs"
              >
                {{ logsLoading ? t('加载中') : `${t('加载更多')} ${selectedLogViewLabel}` }}
              </button>
              <div v-else class="log-limit-note">{{ t('已显示') }} {{ visibleLogCount }} {{ t('条') }} {{ selectedLogViewLabel }}</div>
            </div>
            <div v-else class="empty-state">{{ t('暂无') }} {{ selectedLogViewLabel }}</div>
          </section>

          <section class="panel">
            <div class="panel-head">
              <div>
                <div class="panel-title-line"><Settings2 :size="18" /><h2>{{ t('日志文件配置') }}</h2></div>
                <p class="muted">{{ t('日志写入 data/logs/embypanel.log，默认 INFO 级别。') }}</p>
              </div>
              <button class="primary" @click="saveLogConfig"><Check :size="15" />{{ t('保存日志配置') }}</button>
            </div>
            <div class="grid log-config-grid">
              <label>
                <span>{{ t('日志级别') }}</span>
                <select v-model="logConfig.level">
                  <option value="debug">{{ t('DEBUG - 调试') }}</option>
                  <option value="info">{{ t('INFO - 信息') }}</option>
                  <option value="warning">{{ t('WARNING - 警告') }}</option>
                  <option value="error">{{ t('ERROR - 错误') }}</option>
                  <option value="critical">{{ t('CRITICAL - 严重') }}</option>
                </select>
              </label>
              <label>
                <span>{{ t('单文件最大 MB') }}</span>
                <input v-model.number="logConfig.max_size_mb" type="number" min="1" max="1024" />
              </label>
              <label>
                <span>{{ t('保留文件数') }}</span>
                <input v-model.number="logConfig.max_backups" type="number" min="1" max="99" />
              </label>
              <label class="log-format-field">
                <span>{{ t('日志格式') }}</span>
                <input v-model="logConfig.format" />
              </label>
            </div>
          </section>
        </section>

        <section v-else class="account-grid">
          <section class="panel">
            <div class="panel-head">
              <div class="panel-title-line"><UserRound :size="18" /><h2>{{ t('账户资料') }}</h2></div>
              <button class="primary" :disabled="savingProfile" @click="saveProfile">
                <Check :size="15" />{{ savingProfile ? t('保存中') : t('保存资料') }}
              </button>
            </div>
            <label>
              <span>{{ t('用户名') }}</span>
              <input v-model="profileForm.username" autocomplete="username" />
            </label>
          </section>

          <section class="panel">
            <div class="panel-head">
              <div class="panel-title-line"><ShieldCheck :size="18" /><h2>{{ t('修改密码') }}</h2></div>
            </div>
            <div class="grid">
              <label>
                <span>{{ t('当前密码') }}</span>
                <input v-model="passwordForm.current_password" type="password" autocomplete="current-password" />
              </label>
              <label>
                <span>{{ t('新密码') }}</span>
                <input v-model="passwordForm.new_password" type="password" autocomplete="new-password" />
              </label>
              <label>
                <span>{{ t('确认新密码') }}</span>
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
                <ShieldCheck :size="15" />{{ changingPassword ? t('修改中') : t('修改密码') }}
              </button>
            </div>
          </section>

          <section class="panel">
            <div class="panel-head">
              <div>
                <div class="panel-title-line"><FileText :size="18" /><h2>{{ t('配置审计') }}</h2></div>
                <p class="muted">{{ t('记录配置、账户、通知、备份恢复等管理操作，不保存敏感明文。') }}</p>
              </div>
              <button class="secondary" @click="refreshAuditLogs"><RefreshCw :size="15" />{{ t('刷新') }}</button>
            </div>
            <div class="audit-toolbar">
              <label>
                <span>{{ t('操作类型') }}</span>
                <select v-model="selectedAuditAction" @change="refreshAuditLogs">
                  <option v-for="action in auditActionOptions" :key="action" :value="action">
                    {{ action === 'all' ? t('全部操作') : action }}
                  </option>
                </select>
              </label>
              <label>
                <span>{{ t('关键词') }}</span>
                <input v-model="auditKeywordFilter" :placeholder="t('搜索管理员 / 操作 / 摘要')" @keyup.enter="refreshAuditLogs" />
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
            <div v-else class="empty-state compact">{{ t('暂无审计记录。') }}</div>
          </section>
        </section>
      </div>
    </section>
  </main>
</template>
