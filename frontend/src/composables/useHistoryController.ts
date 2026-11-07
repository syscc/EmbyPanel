import { computed, ref, type ComputedRef, type Ref } from 'vue'

type Api = <T>(path: string, init?: RequestInit) => Promise<T>
type Translate = (source: string) => string

export const HISTORY_METRICS = [
  'requests',
  'redirects',
  'cache_hits',
  'blocks',
  'errors',
  'redirect_success_rate',
  'latency_p95',
  'transcode_rate',
  'bandwidth',
] as const
export type HistoryMetric = (typeof HISTORY_METRICS)[number]
export const HISTORY_DIMENSIONS = ['server', 'user', 'media', 'client'] as const
export type HistoryDimension = (typeof HISTORY_DIMENSIONS)[number]
export const HISTORY_DAY_OPTIONS = [7, 30, 90, 365] as const
export type HistoryDayOption = (typeof HISTORY_DAY_OPTIONS)[number]

export interface HistoryPoint {
  date: string
  metric: string
  value: number
  dimension_key: string | null
  dimension_label: string | null
  server_id: string | null
  server_name: string | null
  sample_count: number
  numerator: number | null
  denominator: number | null
  updated_at_ms: number
}
export interface HistoryCapability {
  available: boolean
  source: string | null
  retention_days: number | null
  reason_code: string | null
  reason: string | null
  caveat: string | null
}
export interface HistoryChartSeries { name: string; values: number[] }
export interface HistoryMetricItem {
  label: string
  value: string
  detail?: string
  tone?: 'default' | 'success' | 'warning' | 'danger'
}
export interface HistoryServerOption { id: string; name: string }

const DEFAULT_METRIC: HistoryMetric = 'requests'
const DEFAULT_DIMENSION: HistoryDimension = 'server'
const DEFAULT_DAYS: HistoryDayOption = 7
const DEFAULT_SERVER = 'all'
const DAY_LABEL_KEYS: Record<HistoryDayOption, string> = {
  7: '近 7 天',
  30: '近 30 天',
  90: '近 90 天',
  365: '近 365 天',
}

function isMetric(value: string): value is HistoryMetric {
  return (HISTORY_METRICS as readonly string[]).includes(value)
}
function isDimension(value: string): value is HistoryDimension {
  return (HISTORY_DIMENSIONS as readonly string[]).includes(value)
}
function clampDays(raw: number): HistoryDayOption {
  if ((HISTORY_DAY_OPTIONS as readonly number[]).includes(raw)) return raw as HistoryDayOption
  if (!Number.isFinite(raw) || raw <= 7) return 7
  if (raw <= 30) return 30
  if (raw <= 90) return 90
  return 365
}
function finiteNonNegative(raw: unknown): number | null {
  const value = typeof raw === 'number' ? raw : typeof raw === 'string' ? Number(raw) : NaN
  return Number.isFinite(value) && value >= 0 ? value : null
}
function optionalString(raw: unknown): string | null {
  return typeof raw === 'string' ? raw : null
}
function normalizePoint(raw: unknown): HistoryPoint | null {
  if (!raw || typeof raw !== 'object') return null
  const row = raw as Record<string, unknown>
  const date = typeof row.date === 'string' ? row.date.trim() : ''
  const value = finiteNonNegative(row.value)
  if (!date || value === null) return null
  return {
    date,
    metric: typeof row.metric === 'string' ? row.metric : '',
    value,
    dimension_key: optionalString(row.dimension_key),
    dimension_label: optionalString(row.dimension_label),
    server_id: optionalString(row.server_id),
    server_name: optionalString(row.server_name),
    sample_count: finiteNonNegative(row.sample_count) ?? 0,
    numerator: finiteNonNegative(row.numerator),
    denominator: finiteNonNegative(row.denominator),
    updated_at_ms: finiteNonNegative(row.updated_at_ms) ?? 0,
  }
}
function normalizeResponse(raw: unknown): { points: HistoryPoint[]; capability: HistoryCapability } {
  const fallback: HistoryCapability = {
    available: true,
    source: 'legacy',
    retention_days: null,
    reason_code: null,
    reason: null,
    caveat: null,
  }
  if (Array.isArray(raw)) {
    return { points: raw.map(normalizePoint).filter((point): point is HistoryPoint => point !== null), capability: fallback }
  }
  if (!raw || typeof raw !== 'object') return { points: [], capability: fallback }
  const body = raw as Record<string, unknown>
  const capabilityRaw = body.capability
  const capability = capabilityRaw && typeof capabilityRaw === 'object'
    ? (() => {
        const value = capabilityRaw as Record<string, unknown>
        return {
          available: value.available !== false,
          source: optionalString(value.source),
          retention_days: finiteNonNegative(value.retention_days),
          reason_code: optionalString(value.reason_code),
          reason: optionalString(value.reason),
          caveat: optionalString(value.caveat),
        }
      })()
    : fallback
  const points = Array.isArray(body.points)
    ? body.points.map(normalizePoint).filter((point): point is HistoryPoint => point !== null)
    : []
  points.sort((left, right) => left.date.localeCompare(right.date) || (left.dimension_key ?? '').localeCompare(right.dimension_key ?? ''))
  return { points, capability }
}
function metricLabel(metric: HistoryMetric, t: Translate): string {
  const labels: Record<HistoryMetric, string> = {
    requests: '今日请求', redirects: '重定向', cache_hits: '缓存命中', blocks: '已拦截', errors: '错误',
    redirect_success_rate: '302 响应率', latency_p95: '延迟 P95', transcode_rate: '转码率', bandwidth: '带宽',
  }
  return t(labels[metric])
}
function dimensionLabel(dimension: HistoryDimension, t: Translate): string {
  return t({ server: '服务器', user: '用户', media: '媒体', client: '客户端' }[dimension])
}
function metricTone(metric: HistoryMetric): HistoryMetricItem['tone'] {
  if (metric === 'cache_hits') return 'success'
  if (metric === 'blocks') return 'warning'
  if (metric === 'errors') return 'danger'
  return 'default'
}
export function formatMetric(metric: HistoryMetric, value: number): string {
  if (!Number.isFinite(value) || value < 0) return '—'
  if (metric === 'redirect_success_rate' || metric === 'transcode_rate') return `${value.toFixed(1)}%`
  if (metric === 'latency_p95') return `${value.toFixed(0)} ms`
  if (metric === 'bandwidth') {
    if (value >= 1_000_000_000) return `${(value / 1_000_000_000).toFixed(2)} Gbps`
    if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(2)} Mbps`
    if (value >= 1_000) return `${(value / 1_000).toFixed(2)} Kbps`
    return `${value.toFixed(0)} bps`
  }
  return value.toLocaleString(undefined, { maximumFractionDigits: 2 })
}
function errorMessage(error: unknown): string {
  if (error instanceof DOMException && error.name === 'AbortError') return ''
  if (error instanceof Error && error.name === 'AbortError') return ''
  return error instanceof Error ? error.message : String(error)
}
function buildPath(metric: HistoryMetric, dimension: HistoryDimension, days: number, server: string): string {
  const params = new URLSearchParams({ metric, dimension, days: String(days) })
  const serverId = server.trim()
  if (serverId && serverId.toLowerCase() !== 'all') params.set('server', serverId)
  return `/api/history?${params.toString()}`
}
function readServers(value: Ref<HistoryServerOption[]> | (() => HistoryServerOption[]) | undefined): HistoryServerOption[] {
  if (!value) return []
  return typeof value === 'function' ? value() : value.value
}

export function useHistoryController(options: {
  api: Api
  translate: Translate
  servers?: Ref<HistoryServerOption[]> | (() => HistoryServerOption[])
}): {
  points: Ref<HistoryPoint[]>
  capability: Ref<HistoryCapability>
  loading: Ref<boolean>
  error: Ref<string>
  metric: Ref<HistoryMetric>
  dimension: Ref<HistoryDimension>
  days: Ref<HistoryDayOption>
  server: Ref<string>
  metricOptions: ComputedRef<Array<{ value: HistoryMetric; label: string }>>
  dimensionOptions: ComputedRef<Array<{ value: HistoryDimension; label: string }>>
  dayOptions: ComputedRef<Array<{ value: HistoryDayOption; label: string }>>
  serverOptions: ComputedRef<Array<{ value: string; label: string }>>
  chartLabels: ComputedRef<string[]>
  chartSeries: ComputedRef<HistoryChartSeries[]>
  metricItems: ComputedRef<HistoryMetricItem[]>
  chartTitle: ComputedRef<string>
  chartAriaLabel: ComputedRef<string>
  loadHistory: () => Promise<void>
  setMetric: (next: string) => void
  setDimension: (next: string) => void
  setDays: (next: number | string) => void
  setServer: (next: string) => void
  dispose: () => void
} {
  const points = ref<HistoryPoint[]>([])
  const capability = ref<HistoryCapability>({ available: true, source: null, retention_days: null, reason_code: null, reason: null, caveat: null })
  const loading = ref(false)
  const error = ref('')
  const metric = ref<HistoryMetric>(DEFAULT_METRIC)
  const dimension = ref<HistoryDimension>(DEFAULT_DIMENSION)
  const days = ref<HistoryDayOption>(DEFAULT_DAYS)
  const server = ref(DEFAULT_SERVER)
  let requestSeq = 0
  let abortController: AbortController | null = null
  let disposed = false

  const metricOptions = computed(() => HISTORY_METRICS.map((value) => ({ value, label: metricLabel(value, options.translate) })))
  const dimensionOptions = computed(() => HISTORY_DIMENSIONS.map((value) => ({ value, label: dimensionLabel(value, options.translate) })))
  const dayOptions = computed(() => (Object.keys(DAY_LABEL_KEYS) as unknown as HistoryDayOption[]).map((value) => ({ value, label: options.translate(DAY_LABEL_KEYS[value]) })))
  const serverOptions = computed(() => {
    const seen = new Set<string>()
    const result: Array<{ value: string; label: string }> = [{ value: 'all', label: options.translate('全部服务器') }]
    for (const item of readServers(options.servers)) {
      const id = item.id.trim()
      if (!id || seen.has(id)) continue
      seen.add(id)
      result.push({ value: id, label: item.name.trim() || id })
    }
    return result
  })
  const chartLabels = computed(() => [...new Set(points.value.map((point) => point.date))].sort())
  const chartSeries = computed<HistoryChartSeries[]>(() => {
    const groups = new Map<string, { name: string; values: number[] }>()
    for (const point of points.value) {
      const key = point.dimension_key || 'all'
      const name = point.dimension_label || point.server_name || key
      if (!groups.has(key)) groups.set(key, { name, values: chartLabels.value.map(() => Number.NaN) })
      const entry = groups.get(key)!
      entry.values[chartLabels.value.indexOf(point.date)] = point.value
    }
    return [...groups.values()]
  })
  const metricItems = computed<HistoryMetricItem[]>(() => {
    const label = metricLabel(metric.value, options.translate)
    if (!capability.value.available) return [{ label, value: options.translate('不可用'), detail: capability.value.reason ? options.translate(capability.value.reason) : options.translate('暂无数据'), tone: 'warning' }]
    if (points.value.length === 0) return [{ label, value: options.translate('暂无数据'), detail: capability.value.reason ? options.translate(capability.value.reason) : options.translate('暂无数据'), tone: 'default' }]
    const byDate = new Map<string, HistoryPoint[]>()
    for (const point of points.value) {
      const rows = byDate.get(point.date) ?? []
      rows.push(point)
      byDate.set(point.date, rows)
    }
    const sampleDays = {
      label: options.translate('样本天数'),
      value: String(byDate.size),
      detail: options.translate(`近 ${days.value} 天`),
    }
    if (metric.value === 'latency_p95') {
      // Quantiles cannot be reconstructed from per-group quantiles.
      return [
        { label, value: '—', detail: options.translate('分组 P95 无法合并为全局 P95') },
        { label: options.translate('分组日 P95 最大值'), value: formatMetric(metric.value, Math.max(...points.value.map((point) => point.value))), detail: options.translate('仅比较已有分组日样本') },
        sampleDays,
      ]
    }
    const weighted = metric.value === 'redirect_success_rate' || metric.value === 'transcode_rate' || metric.value === 'bandwidth'
    if (weighted) {
      const aggregate = (rows: HistoryPoint[]) => {
        if (rows.some((point) => point.numerator === null || point.denominator === null || point.denominator <= 0)) return Number.NaN
        const numerator = rows.reduce((sum, point) => sum + point.numerator!, 0)
        const denominator = rows.reduce((sum, point) => sum + point.denominator!, 0)
        return numerator / denominator * (metric.value === 'bandwidth' ? 1 : 100)
      }
      const daily = [...byDate.values()].map(aggregate)
      return [
        { label, value: formatMetric(metric.value, aggregate(points.value)), detail: options.translate('按样本加权（仅当前返回数据）') },
        { label: options.translate('日加权峰值'), value: formatMetric(metric.value, Math.max(...daily)), detail: options.translate('缺少权重时不计算汇总') },
        sampleDays,
      ]
    }
    const daily = [...byDate.values()].map((rows) => rows.reduce((sum, point) => sum + point.value, 0))
    const total = daily.reduce((sum, value) => sum + value, 0)
    const peak = Math.max(...daily)
    return [
      { label, value: formatMetric(metric.value, total), detail: options.translate('合计'), tone: metricTone(metric.value) },
      { label: options.translate('日均'), value: formatMetric(metric.value, total / byDate.size), detail: options.translate('按有数据的日期计算，未采样日期不计入') },
      { label: options.translate('峰值'), value: formatMetric(metric.value, peak), detail: options.translate('按日期合计的峰值'), tone: peak > 0 ? 'warning' : 'default' },
      sampleDays,
    ]
  })
  const chartTitle = computed(() => `${metricLabel(metric.value, options.translate)} · ${dimensionLabel(dimension.value, options.translate)}`)
  const chartAriaLabel = computed(() => chartTitle.value)

  async function loadHistory() {
    if (disposed) return
    const seq = ++requestSeq
    abortController?.abort()
    const controller = new AbortController()
    abortController = controller
    loading.value = true
    error.value = ''
    try {
      const raw = await options.api<unknown>(buildPath(metric.value, dimension.value, days.value, server.value), { signal: controller.signal })
      if (disposed || seq !== requestSeq) return
      const normalized = normalizeResponse(raw)
      points.value = normalized.points
      capability.value = normalized.capability
    } catch (err) {
      if (disposed || seq !== requestSeq || controller.signal.aborted) return
      const message = errorMessage(err)
      if (!message) return
      error.value = message
      points.value = []
    } finally {
      if (!disposed && seq === requestSeq) {
        loading.value = false
        if (abortController === controller) abortController = null
      }
    }
  }
  function setMetric(next: string) {
    const value = next.trim()
    metric.value = isMetric(value) ? value : DEFAULT_METRIC
  }
  function setDimension(next: string) {
    const value = next.trim()
    dimension.value = isDimension(value) ? value : DEFAULT_DIMENSION
  }
  function setDays(next: number | string) { days.value = clampDays(typeof next === 'number' ? next : Number(next)) }
  function setServer(next: string) { server.value = next.trim() || DEFAULT_SERVER }
  function dispose() {
    if (disposed) return
    disposed = true
    requestSeq += 1
    abortController?.abort()
    abortController = null
    loading.value = false
  }
  return { points, capability, loading, error, metric, dimension, days, server, metricOptions, dimensionOptions, dayOptions, serverOptions, chartLabels, chartSeries, metricItems, chartTitle, chartAriaLabel, loadHistory, setMetric, setDimension, setDays, setServer, dispose }
}
