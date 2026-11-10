import { ref, watch, type Ref } from 'vue'
import type { ActionDialogRequest } from '@/composables/useActionDialog'

export type DiagnosisConnectionObservation = 'not_observed' | 'observed' | 'partial'

/** Matches backend `DiagnosisStep` in src/diagnostics.rs */
export interface DiagnosisStep {
  name: string
  started_at_ms?: number | null
  finished_at_ms?: number | null
  duration_ms: number
  details: string
  state?: 'recorded' | 'skipped' | 'failed' | 'cancelled' | null
  reason?: string | null
  upstream_status?: number | null
  error_kind?: string | null
  response_headers_ms?: number | null
  response_body_ms?: number | null
  name_resolution_ms?: number | null
  name_resolution_calls?: number | null
  connection_setup_ms?: number | null
  connection_attempts?: number | null
  connection_observation?: DiagnosisConnectionObservation | null
}

export interface DiagnosisContext {
  server_name?: string | null
  media_name?: string | null
  series_name?: string | null
  season_number?: number | null
  episode_number?: number | null
  media_source_id?: string | null
  user_name?: string | null
  client_name?: string | null
  device_name?: string | null
  device_id?: string | null
  source_ip?: string | null
  method?: string | null
  request_path?: string | null
  range?: string | null
}

export type DiagnosisOutcome = 'redirect' | 'proxy' | 'failed' | 'cancelled' | 'blocked'
export interface DiagnosisResult {
  outcome?: DiagnosisOutcome | null
  status_code?: number | null
  target_host?: string | null
  reason?: string | null
  error_kind?: string | null
  upstream_status?: number | null
}
export interface DiagnosisCache {
  status?: 'hit' | 'miss' | 'disabled' | 'not_checked' | null
  stored?: boolean | null
  reason?: string | null
  ttl_seconds?: number | null
  remaining_ttl_ms?: number | null
  target_host?: string | null
}
export interface DiagnosisPaths {
  original?: string | null
  mapped?: string | null
  matched_rules?: number[]
  resolved?: string | null
  final_url?: string | null
}
export interface DiagnosisTiming {
  slowest_step?: string | null
  slowest_step_ms?: number | null
  unaccounted_ms?: number | null
  redirect_count?: number | null
  /** System resolver wait; overlaps connection setup and must not be added to trace duration. */
  name_resolution_ms?: number | null
  name_resolution_calls?: number | null
  /** Observed attempt total, not pure TCP/TLS or proof of use by this response. */
  connection_setup_ms?: number | null
  connection_attempts?: number | null
  connection_observation?: DiagnosisConnectionObservation | null
}

/** Matches backend `DiagnosisRecord` in src/diagnostics.rs */
export interface DiagnosisRecord {
  request_id: string
  /** Unix epoch milliseconds; older backends may omit timing. */
  started_at_ms?: number | null
  finished_at_ms?: number | null
  /** Whole-trace monotonic duration, not a sum of step durations. */
  duration_ms?: number | null
  server_id: string
  media_id: string
  ua: string
  steps: DiagnosisStep[]
  cache_hit: boolean
  failure_node: string | null
  sanitized_errors: string[]
  context?: DiagnosisContext | null
  result?: DiagnosisResult | null
  cache?: DiagnosisCache | null
  paths?: DiagnosisPaths | null
  timing?: DiagnosisTiming | null
}

/** UI filters; local times map to the backend since_ms/until_ms query fields. */
export interface DiagnosticsQuery {
  request_id: string
  server_id: string
  media_id: string
  outcome: DiagnosisOutcome | ''
  user: string
  keyword: string
  /** Local datetime-local values; serialized as Unix milliseconds. */
  since: string
  until: string
  min_duration_ms: number | ''
  /** default 50, max 100 on server */
  limit: number
  /** 1-based page */
  page: number
}

/** Matches backend `DiagnosticsResponse` in src/diagnostics_api.rs */
export interface DiagnosticsResponse {
  records: DiagnosisRecord[]
  can_global_cleanup: boolean
  total?: number
  has_more?: boolean
  storage?: DiagnosticsStorage
}

export interface DiagnosticsStorage {
  persistent: boolean
  retention_days: number
  max_records: number
  memory_capacity: number
}

/** Matches backend `CleanDiagnosticsRequest` */
export interface CleanDiagnosticsRequest {
  confirm: 'yes'
}

type Api = <T>(path: string, init?: RequestInit) => Promise<T>
type Translate = (source: string) => string
type ShowNotice = (message: string) => void
type ClearNotice = () => void
type ConfirmAction = (options: Omit<ActionDialogRequest, 'kind'>) => Promise<boolean>

const DEFAULT_LIMIT = 50
const MAX_LIMIT = 100

function normalizeLimit(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_LIMIT
  return Math.min(MAX_LIMIT, Math.max(1, Math.trunc(value)))
}

function normalizePage(value: number): number {
  if (!Number.isFinite(value)) return 1
  return Math.max(1, Math.trunc(value))
}

function isAbortError(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false
  const name = 'name' in error ? String((error as { name?: unknown }).name ?? '') : ''
  return name === 'AbortError' || name === 'TimeoutError'
}

function parseDiagnosticsResponse(value: unknown): DiagnosticsResponse {
  if (!value || typeof value !== 'object') {
    return { records: [], can_global_cleanup: false }
  }

  const response = value as Partial<DiagnosticsResponse>
  const records = Array.isArray(response.records) ? response.records : []
  const storage = response.storage
  return {
    records,
    can_global_cleanup: Array.isArray(response.records) && response.can_global_cleanup === true,
    total: Number.isSafeInteger(response.total) && response.total! >= 0 ? response.total : undefined,
    has_more: typeof response.has_more === 'boolean' ? response.has_more : undefined,
    storage: storage && typeof storage.persistent === 'boolean'
      && [storage.retention_days, storage.max_records, storage.memory_capacity].every((n) => Number.isSafeInteger(n) && n >= 0)
      ? storage : undefined,
  }
}

function defaultQuery(): DiagnosticsQuery {
  return { request_id: '', server_id: '', media_id: '', outcome: '', user: '', keyword: '',
    since: '', until: '', min_duration_ms: '', limit: DEFAULT_LIMIT, page: 1 }
}

function localTimestamp(value: string): number | null {
  if (!value) return null
  const timestamp = new Date(value).getTime()
  if (!Number.isSafeInteger(timestamp) || timestamp <= 0) throw new Error('诊断筛选时间无效')
  return timestamp
}

function buildDiagnosticsPath(query: DiagnosticsQuery): string {
  const params = new URLSearchParams()
  const requestId = query.request_id.trim()
  const serverId = query.server_id.trim()
  const mediaId = query.media_id.trim()
  const limit = normalizeLimit(query.limit)
  const page = normalizePage(query.page)

  if (requestId) params.set('request_id', requestId)
  if (serverId) params.set('server_id', serverId)
  if (mediaId) params.set('media_id', mediaId)
  for (const key of ['outcome', 'user', 'keyword'] as const) {
    const value = query[key].trim()
    if (value) params.set(key, value)
  }
  const since = localTimestamp(query.since)
  const until = localTimestamp(query.until)
  if (since !== null && until !== null && since > until) throw new Error('诊断开始时间不能晚于结束时间')
  if (since !== null) params.set('since_ms', String(since))
  if (until !== null) params.set('until_ms', String(until))
  if (query.min_duration_ms !== '') {
    if (!Number.isSafeInteger(query.min_duration_ms) || query.min_duration_ms < 0) throw new Error('最小耗时必须是非负整数')
    params.set('min_duration_ms', String(query.min_duration_ms))
  }
  params.set('limit', String(limit))
  params.set('page', String(page))
  return `/api/diagnostics?${params.toString()}`
}

/** Share exports deliberately omit all free text, paths, hosts and user/device identifiers. */
export function diagnosisShareRecord(record: DiagnosisRecord) {
  const states = new Set(['recorded', 'skipped', 'failed', 'cancelled'])
  const outcomes = new Set(['redirect', 'proxy', 'failed', 'cancelled', 'blocked'])
  const cacheStates = new Set(['hit', 'miss', 'disabled', 'not_checked'])
  const connectionObservations = new Set(['not_observed', 'observed', 'partial'])
  const stepNames = new Set(['请求上下文', '播放策略', 'Emby Path', '映射', 'OpenList', 'raw_url', '缓存', '302', '内部跳转', 'Emby Proxy'])
  const metric = (value: number | null | undefined) =>
    typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null
  return {
    request_id: /^diag-[a-zA-Z0-9-]{1,120}$/.test(record.request_id) ? record.request_id : null,
    started_at_ms: metric(record.started_at_ms),
    finished_at_ms: metric(record.finished_at_ms),
    duration_ms: metric(record.duration_ms),
    cache_hit: record.cache_hit,
    result: {
      outcome: outcomes.has(record.result?.outcome ?? '') ? record.result!.outcome : null,
      status_code: metric(record.result?.status_code),
      upstream_status: metric(record.result?.upstream_status),
    },
    cache: {
      status: cacheStates.has(record.cache?.status ?? '') ? record.cache!.status : null,
      stored: typeof record.cache?.stored === 'boolean' ? record.cache.stored : null,
      ttl_seconds: metric(record.cache?.ttl_seconds),
      remaining_ttl_ms: metric(record.cache?.remaining_ttl_ms),
    },
    timing: {
      slowest_step_ms: metric(record.timing?.slowest_step_ms),
      unaccounted_ms: metric(record.timing?.unaccounted_ms),
      redirect_count: metric(record.timing?.redirect_count),
      name_resolution_ms: metric(record.timing?.name_resolution_ms),
      name_resolution_calls: metric(record.timing?.name_resolution_calls),
      connection_setup_ms: metric(record.timing?.connection_setup_ms),
      connection_attempts: metric(record.timing?.connection_attempts),
      connection_observation: connectionObservations.has(record.timing?.connection_observation ?? '') ? record.timing!.connection_observation : null,
    },
    steps: record.steps.map((step) => ({
      name: stepNames.has(step.name) ? step.name : null,
      state: states.has(step.state ?? '') ? step.state : null,
      started_at_ms: metric(step.started_at_ms), finished_at_ms: metric(step.finished_at_ms),
      duration_ms: metric(step.duration_ms), upstream_status: metric(step.upstream_status),
      response_headers_ms: metric(step.response_headers_ms), response_body_ms: metric(step.response_body_ms),
      name_resolution_ms: metric(step.name_resolution_ms), name_resolution_calls: metric(step.name_resolution_calls),
      connection_setup_ms: metric(step.connection_setup_ms), connection_attempts: metric(step.connection_attempts),
      connection_observation: connectionObservations.has(step.connection_observation ?? '') ? step.connection_observation : null,
    })),
  }
}

export function useDiagnosticsController(options: {
  api: Api
  translate: Translate
  showNotice: ShowNotice
  clearNotice: ClearNotice
  confirmAction: ConfirmAction
}) {
  const diagnostics = ref<DiagnosisRecord[]>([])
  const loading = ref(false)
  const cleaning = ref(false)
  const canGlobalCleanup = ref(false)
  const error = ref('')
  const query = ref<DiagnosticsQuery>(defaultQuery())
  const loadedPage = ref<number | null>(null)
  const total = ref<number | null>(null)
  const storage = ref<DiagnosticsStorage | null>(null)
  const hasNextPage = ref(false)

  let disposed = false
  let loadSeq = 0
  let abortController: AbortController | null = null
  let cleanAbortController: AbortController | null = null

  const stopFilterWatch = watch(
    () => [query.value.request_id, query.value.server_id, query.value.media_id, query.value.limit,
      query.value.outcome, query.value.user, query.value.keyword, query.value.since, query.value.until, query.value.min_duration_ms],
    () => { query.value.page = 1 },
    { flush: 'sync' },
  )

  function dispose() {
    stopFilterWatch()
    disposed = true
    abortController?.abort()
    abortController = null
    cleanAbortController?.abort()
    cleanAbortController = null
  }

  async function loadDiagnostics(opts?: { suppressClearNotice?: boolean }) {
    if (disposed) return
    const seq = ++loadSeq
    abortController?.abort()
    const controller = new AbortController()
    abortController = controller

    loading.value = true
    canGlobalCleanup.value = false
    error.value = ''
    // Default clears stale notices; clean-success refresh can opt out.
    if (!opts?.suppressClearNotice) {
      options.clearNotice()
    }

    const activeQuery: DiagnosticsQuery = {
      ...query.value,
      limit: normalizeLimit(query.value.limit),
      page: normalizePage(query.value.page),
    }
    query.value.limit = activeQuery.limit
    query.value.page = activeQuery.page

    try {
      const response = parseDiagnosticsResponse(await options.api<unknown>(buildDiagnosticsPath(activeQuery), {
        signal: controller.signal,
      }))
      if (disposed || seq !== loadSeq) return
      diagnostics.value = response.records
      loadedPage.value = activeQuery.page
      canGlobalCleanup.value = response.can_global_cleanup
      total.value = response.total ?? null
      storage.value = response.storage ?? null
      hasNextPage.value = response.has_more ?? (response.records.length >= activeQuery.limit)
    } catch (e) {
      if (disposed || seq !== loadSeq || controller.signal.aborted || isAbortError(e)) return
      error.value = options.translate(e instanceof Error ? e.message : String(e))
      loadedPage.value = null
      total.value = null
      storage.value = null
      diagnostics.value = []
      canGlobalCleanup.value = false
      hasNextPage.value = false
    } finally {
      if (!disposed && seq === loadSeq) {
        loading.value = false
        if (abortController === controller) abortController = null
      }
    }
  }

  async function applyFilters() {
    await goToPage(query.value.page)
  }

  async function resetFilters() {
    query.value = defaultQuery()
    await loadDiagnostics()
  }

  async function goToPage(page: number) {
    query.value.page = normalizePage(page)
    await loadDiagnostics()
  }

  async function nextPage() {
    if (!hasNextPage.value || loading.value) return
    await goToPage(query.value.page + 1)
  }

  async function prevPage() {
    if (query.value.page <= 1 || loading.value) return
    await goToPage(query.value.page - 1)
  }

  async function cleanDiagnostics() {
    if (disposed || cleaning.value || !canGlobalCleanup.value) return
    const confirmed = await options.confirmAction({
      title: options.translate('清空诊断记录'),
      description: options.translate('确定清空全部诊断记录吗？'),
      confirmText: options.translate('清空'),
      cancelText: options.translate('取消'),
      tone: 'danger',
    })
    if (!confirmed || disposed || !canGlobalCleanup.value) return

    cleaning.value = true
    error.value = ''
    const controller = new AbortController()
    cleanAbortController?.abort()
    cleanAbortController = controller
    try {
      const body: CleanDiagnosticsRequest = { confirm: 'yes' }
      await options.api<null>('/api/diagnostics/clean', {
        method: 'POST',
        body: JSON.stringify(body),
        signal: controller.signal,
      })
      if (disposed) return
      options.showNotice(options.translate('诊断记录已清空'))
      query.value.page = 1
      // Keep the success notice while reloading the now-empty list.
      await loadDiagnostics({ suppressClearNotice: true })
    } catch (e) {
      if (disposed || controller.signal.aborted || isAbortError(e)) return
      error.value = e instanceof Error ? e.message : String(e)
    } finally {
      if (cleanAbortController === controller) cleanAbortController = null
      if (!disposed) cleaning.value = false
    }
  }

  return {
    diagnostics,
    loading,
    cleaning,
    canGlobalCleanup,
    error,
    query,
    hasNextPage,
    loadedPage,
    total,
    storage,
    loadDiagnostics,
    applyFilters,
    resetFilters,
    goToPage,
    nextPage,
    prevPage,
    cleanDiagnostics,
    dispose,
  }
}

export type DiagnosticsController = {
  diagnostics: Ref<DiagnosisRecord[]>
  loading: Ref<boolean>
  cleaning: Ref<boolean>
  canGlobalCleanup: Ref<boolean>
  error: Ref<string>
  query: Ref<DiagnosticsQuery>
  hasNextPage: Ref<boolean>
  loadedPage: Ref<number | null>
  total: Ref<number | null>
  storage: Ref<DiagnosticsStorage | null>
  loadDiagnostics: (opts?: { suppressClearNotice?: boolean }) => Promise<void>
  applyFilters: () => Promise<void>
  resetFilters: () => Promise<void>
  goToPage: (page: number) => Promise<void>
  nextPage: () => Promise<void>
  prevPage: () => Promise<void>
  cleanDiagnostics: () => Promise<void>
  dispose: () => void
}
