import { ref, watch, type Ref } from 'vue'
import type { ActionDialogRequest } from '@/composables/useActionDialog'

/** Matches backend `DiagnosisStep` in src/diagnostics.rs */
export interface DiagnosisStep {
  name: string
  duration_ms: number
  details: string
}

/** Matches backend `DiagnosisRecord` in src/diagnostics.rs */
export interface DiagnosisRecord {
  request_id: string
  server_id: string
  media_id: string
  ua: string
  steps: DiagnosisStep[]
  cache_hit: boolean
  failure_node: string | null
  sanitized_errors: string[]
}

/** Matches backend `DiagnosticsQuery` in src/diagnostics_api.rs */
export interface DiagnosticsQuery {
  request_id: string
  server_id: string
  media_id: string
  /** default 50, max 100 on server */
  limit: number
  /** 1-based page */
  page: number
}

/** Matches backend `DiagnosticsResponse` in src/diagnostics_api.rs */
export interface DiagnosticsResponse {
  records: DiagnosisRecord[]
  can_global_cleanup: boolean
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

  const response = value as { records?: unknown; can_global_cleanup?: unknown }
  const records = Array.isArray(response.records) ? response.records as DiagnosisRecord[] : []
  return {
    records,
    can_global_cleanup: Array.isArray(response.records) && response.can_global_cleanup === true,
  }
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
  params.set('limit', String(limit))
  params.set('page', String(page))
  return `/api/diagnostics?${params.toString()}`
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
  const query = ref<DiagnosticsQuery>({
    request_id: '',
    server_id: '',
    media_id: '',
    limit: DEFAULT_LIMIT,
    page: 1,
  })
  const hasNextPage = ref(false)

  let disposed = false
  let loadSeq = 0
  let abortController: AbortController | null = null
  let cleanAbortController: AbortController | null = null

  const stopFilterWatch = watch(
    () => [query.value.request_id, query.value.server_id, query.value.media_id, query.value.limit],
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
      request_id: query.value.request_id,
      server_id: query.value.server_id,
      media_id: query.value.media_id,
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
      canGlobalCleanup.value = response.can_global_cleanup
      hasNextPage.value = response.records.length >= activeQuery.limit
    } catch (e) {
      if (disposed || seq !== loadSeq || controller.signal.aborted || isAbortError(e)) return
      error.value = e instanceof Error ? e.message : String(e)
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
    query.value = {
      request_id: '',
      server_id: '',
      media_id: '',
      limit: DEFAULT_LIMIT,
      page: 1,
    }
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
  loadDiagnostics: (opts?: { suppressClearNotice?: boolean }) => Promise<void>
  applyFilters: () => Promise<void>
  resetFilters: () => Promise<void>
  goToPage: (page: number) => Promise<void>
  nextPage: () => Promise<void>
  prevPage: () => Promise<void>
  cleanDiagnostics: () => Promise<void>
  dispose: () => void
}
