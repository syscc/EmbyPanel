import { computed, ref, type ComputedRef, type Ref } from 'vue'
import type { ActionDialogRequest } from '@/composables/useActionDialog'

type Api = <T>(path: string, init?: RequestInit) => Promise<T>
type Translate = (source: string) => string
type ShowNotice = (message: string) => void
type ConfirmAction = (
  options: Omit<ActionDialogRequest, 'kind'>,
) => Promise<boolean>

/** Mirrors backend CacheStats (capacity unit = entries, never bytes). */
export interface CacheStats {
  capacity: number
  maxCapacity: number
  unit: string
  hits: number
  misses: number
  hitRate: number
}

/** Mirrors backend CacheEntryView — no raw key/URL/UA. */
export interface CacheEntryView {
  id: string
  serverId: string
  mediaId: string
  sourceId: string
  legacy: boolean
}

export type CacheCleanupScope = 'all' | 'server' | 'media' | 'server_media'

export interface CacheCleanupResponse {
  ok: boolean
  deleted: number
  scope: string
  serverId?: string
  mediaId?: string
}

const EMPTY_STATS: CacheStats = {
  capacity: 0,
  maxCapacity: 0,
  unit: 'entries',
  hits: 0,
  misses: 0,
  hitRate: 0,
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function isAbortError(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false
  const name = 'name' in error ? String((error as { name?: unknown }).name ?? '') : ''
  return name === 'AbortError'
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === 'object'
}

function normalizeStats(raw: unknown): CacheStats | null {
  if (!isRecord(raw)) return null
  const { capacity, maxCapacity, unit, hits, misses, hitRate } = raw
  if (
    typeof capacity !== 'number' ||
    !Number.isSafeInteger(capacity) ||
    capacity < 0 ||
    typeof maxCapacity !== 'number' ||
    !Number.isSafeInteger(maxCapacity) ||
    maxCapacity < 0 ||
    unit !== 'entries' ||
    typeof hits !== 'number' ||
    !Number.isSafeInteger(hits) ||
    hits < 0 ||
    typeof misses !== 'number' ||
    !Number.isSafeInteger(misses) ||
    misses < 0 ||
    typeof hitRate !== 'number' ||
    !Number.isFinite(hitRate) ||
    hitRate < 0 ||
    hitRate > 1
  ) {
    return null
  }
  return { capacity, maxCapacity, unit, hits, misses, hitRate }
}

function normalizeStatsResponse(raw: unknown): CacheStats | null {
  if (!isRecord(raw) || raw.canViewGlobalStats !== true) return null
  return normalizeStats(raw.stats)
}

function normalizeEntries(raw: unknown): CacheEntryView[] {
  if (!Array.isArray(raw)) return []
  const views: CacheEntryView[] = []
  for (const item of raw) {
    if (!item || typeof item !== 'object') continue
    const row = item as Record<string, unknown>
    const id = typeof row.id === 'string' ? row.id : ''
    if (!id) continue
    views.push({
      id,
      serverId: typeof row.serverId === 'string' ? row.serverId : '',
      mediaId: typeof row.mediaId === 'string' ? row.mediaId : '',
      sourceId: typeof row.sourceId === 'string' ? row.sourceId : '',
      legacy: Boolean(row.legacy),
    })
  }
  return views
}

const ENTRY_PAGE_SIZE = 100
const MAX_ENTRY_PAGES = 5

function entriesPath(search: string, page: number): string {
  const params = new URLSearchParams({
    page: String(page),
    limit: String(ENTRY_PAGE_SIZE),
  })
  const keyword = search.trim()
  if (keyword) params.set('search', keyword)
  return `/api/cache/entries?${params.toString()}`
}

export function useCacheManagementController(options: {
  api: Api
  translate: Translate
  showNotice: ShowNotice
  confirmAction: ConfirmAction
}) {
  const stats = ref<CacheStats>({ ...EMPTY_STATS })
  const canViewGlobalStats = ref(false)
  const canCleanupAll = ref(false)
  const entries = ref<CacheEntryView[]>([])
  const loading = ref(false)
  const cleaning = ref(false)
  const error = ref('')
  const search = ref('')
  const serverId = ref('')
  const mediaId = ref('')
  const page = ref(1)
  const hasNextPage = ref(false)

  let requestSeq = 0
  let abortController: AbortController | null = null
  let cleanupAbortController: AbortController | null = null
  let disposed = false

  const hitRatePercent = computed(() => `${(stats.value.hitRate * 100).toFixed(1)}%`)
  const canCleanupServer = computed(() => serverId.value.trim().length > 0)
  const canCleanupMedia = computed(() => mediaId.value.trim().length > 0)
  const canCleanupServerMedia = computed(
    () => canCleanupServer.value && canCleanupMedia.value,
  )
  const canNextPage = computed(
    () => hasNextPage.value && page.value < MAX_ENTRY_PAGES,
  )

  function clearGlobalStats() {
    canViewGlobalStats.value = false
    canCleanupAll.value = false
    stats.value = { ...EMPTY_STATS }
  }

  function startLoad() {
    const seq = ++requestSeq
    abortController?.abort()
    const controller = new AbortController()
    abortController = controller
    loading.value = true
    error.value = ''
    return { seq, controller }
  }

  function isCurrentLoad(seq: number, controller: AbortController) {
    return !disposed && seq === requestSeq && !controller.signal.aborted
  }

  function finishLoad(seq: number, controller: AbortController) {
    if (!isCurrentLoad(seq, controller)) return
    loading.value = false
    if (abortController === controller) abortController = null
  }

  function cancelLoad() {
    requestSeq += 1
    abortController?.abort()
    abortController = null
    loading.value = false
  }

  async function refresh() {
    if (disposed) return
    clearGlobalStats()
    const { seq, controller } = startLoad()
    const path = entriesPath(search.value, page.value)
    try {
      const [statsResult, entriesResult] = await Promise.allSettled([
        options.api<unknown>('/api/cache/stats', {
          signal: controller.signal,
        }),
        options.api<unknown>(path, { signal: controller.signal }),
      ])
      if (!isCurrentLoad(seq, controller)) return

      const errors: string[] = []
      if (statsResult.status === 'fulfilled') {
        canCleanupAll.value =
          isRecord(statsResult.value) && statsResult.value.canCleanupAll === true
        const parsedStats = normalizeStatsResponse(statsResult.value)
        if (parsedStats) {
          stats.value = parsedStats
          canViewGlobalStats.value = true
        }
      } else if (!isAbortError(statsResult.reason)) {
        errors.push(errorMessage(statsResult.reason))
      }
      if (entriesResult.status === 'fulfilled') {
        entries.value = normalizeEntries(entriesResult.value)
        hasNextPage.value = entries.value.length >= ENTRY_PAGE_SIZE
      } else if (!isAbortError(entriesResult.reason)) {
        entries.value = []
        hasNextPage.value = false
        errors.push(errorMessage(entriesResult.reason))
      }
      error.value = errors.join('；')
    } finally {
      finishLoad(seq, controller)
    }
  }

  async function applySearch() {
    if (disposed) return
    page.value = 1
    const { seq, controller } = startLoad()
    const path = entriesPath(search.value, page.value)
    try {
      const raw = await options.api<unknown>(path, { signal: controller.signal })
      if (!isCurrentLoad(seq, controller)) return
      entries.value = normalizeEntries(raw)
      hasNextPage.value = entries.value.length >= ENTRY_PAGE_SIZE
    } catch (err) {
      if (!isCurrentLoad(seq, controller) || isAbortError(err)) return
      error.value = errorMessage(err)
      // Do not show results from a previous query under a failed new search.
      entries.value = []
      hasNextPage.value = false
    } finally {
      finishLoad(seq, controller)
    }
  }

  async function cleanup(scope: CacheCleanupScope) {
    if (disposed || cleaning.value) return
    if (scope === 'all' && !canCleanupAll.value) {
      error.value = options.translate('无权限')
      return
    }
    const sid = serverId.value.trim()
    const mid = mediaId.value.trim()

    const body: {
      scope: CacheCleanupScope
      serverId?: string
      mediaId?: string
    } = { scope }

    let description: string
    if (scope === 'all') {
      description = options.translate(
        '确定清理全部直链缓存条目吗？此操作不可撤销。',
      )
    } else if (scope === 'server') {
      if (!sid) {
        error.value = options.translate('按服务器清理需要服务器 ID')
        return
      }
      body.serverId = sid
      description = options.translate('确定清理该服务器的直链缓存吗？')
    } else if (scope === 'media') {
      if (!mid) {
        error.value = options.translate('按媒体清理需要媒体 ID')
        return
      }
      body.mediaId = mid
      description = options.translate('确定清理该媒体的直链缓存吗？')
    } else {
      if (!sid || !mid) {
        error.value = options.translate(
          '按服务器和媒体清理需要同时填写服务器 ID 与媒体 ID',
        )
        return
      }
      body.serverId = sid
      body.mediaId = mid
      description = options.translate(
        '确定清理该服务器下指定媒体的直链缓存吗？',
      )
    }

    cleaning.value = true
    error.value = ''
    try {
      const confirmed = await options.confirmAction({
        title: options.translate('清理缓存'),
        description,
        confirmText: options.translate('清理'),
        cancelText: options.translate('取消'),
        tone: scope === 'all' ? 'danger' : 'warning',
      })
      if (!confirmed || disposed) return

      cancelLoad()
      const controller = new AbortController()
      cleanupAbortController = controller
      const result = await options.api<CacheCleanupResponse>('/api/cache/cleanup', {
        method: 'POST',
        body: JSON.stringify(body),
        signal: controller.signal,
      })
      if (disposed || controller.signal.aborted) return
      const deleted =
        result && typeof result.deleted === 'number' ? result.deleted : 0
      options.showNotice(
        `${options.translate('缓存已清理')} (${options.translate('删除')}: ${deleted})`,
      )
      page.value = 1
      // A cleanup success and a subsequent refresh failure are independent states.
      await refresh()
    } catch (err) {
      if (disposed || isAbortError(err)) return
      error.value = errorMessage(err)
    } finally {
      cleanupAbortController = null
      if (!disposed) cleaning.value = false
    }
  }

  async function goToPage(nextPage: number) {
    if (disposed || loading.value || cleaning.value) return
    page.value = Math.max(1, Math.floor(nextPage))
    const { seq, controller } = startLoad()
    const path = entriesPath(search.value, page.value)
    try {
      const raw = await options.api<unknown>(path, { signal: controller.signal })
      if (!isCurrentLoad(seq, controller)) return
      entries.value = normalizeEntries(raw)
      hasNextPage.value = entries.value.length >= ENTRY_PAGE_SIZE
    } catch (err) {
      if (!isCurrentLoad(seq, controller) || isAbortError(err)) return
      error.value = errorMessage(err)
      entries.value = []
      hasNextPage.value = false
    } finally {
      finishLoad(seq, controller)
    }
  }

  function nextPage() {
    if (!hasNextPage.value || page.value >= MAX_ENTRY_PAGES) return Promise.resolve()
    return goToPage(page.value + 1)
  }

  function prevPage() {
    if (page.value <= 1) return Promise.resolve()
    return goToPage(page.value - 1)
  }

  function dispose() {
    if (disposed) return
    disposed = true
    cancelLoad()
    cleanupAbortController?.abort()
    cleanupAbortController = null
    cleaning.value = false
  }

  function cleanupAll() {
    return cleanup('all')
  }

  function cleanupByServer() {
    return cleanup('server')
  }

  function cleanupByMedia() {
    return cleanup('media')
  }

  function cleanupByServerMedia() {
    return cleanup('server_media')
  }

  return {
    stats,
    canViewGlobalStats,
    canCleanupAll,
    entries,
    loading,
    cleaning,
    error,
    search,
    serverId,
    mediaId,
    page,
    hasNextPage,
    canNextPage,
    hitRatePercent,
    canCleanupServer,
    canCleanupMedia,
    canCleanupServerMedia,
    refresh,
    applySearch,
    nextPage,
    prevPage,
    cleanupAll,
    cleanupByServer,
    cleanupByMedia,
    cleanupByServerMedia,
    dispose,
  }
}

export type CacheManagementController = {
  stats: Ref<CacheStats>
  canViewGlobalStats: Ref<boolean>
  canCleanupAll: Ref<boolean>
  entries: Ref<CacheEntryView[]>
  loading: Ref<boolean>
  cleaning: Ref<boolean>
  error: Ref<string>
  search: Ref<string>
  serverId: Ref<string>
  mediaId: Ref<string>
  page: Ref<number>
  hasNextPage: Ref<boolean>
  canNextPage: ComputedRef<boolean>
  hitRatePercent: ComputedRef<string>
  canCleanupServer: ComputedRef<boolean>
  canCleanupMedia: ComputedRef<boolean>
  canCleanupServerMedia: ComputedRef<boolean>
  refresh: () => Promise<void>
  applySearch: () => Promise<void>
  nextPage: () => Promise<void>
  prevPage: () => Promise<void>
  cleanupAll: () => Promise<void>
  cleanupByServer: () => Promise<void>
  cleanupByMedia: () => Promise<void>
  cleanupByServerMedia: () => Promise<void>
  dispose: () => void
}
