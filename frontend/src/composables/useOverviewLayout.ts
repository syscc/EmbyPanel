import { computed, ref } from 'vue'

export type OverviewModuleId = 'media' | 'health' | 'operations' | 'rate-limit' | 'playing'

/**
 * Fixed module metadata. Reordering and the visibility switches only change the
 * position of a module; the grid span of every module is contractual and never
 * changes. `label` is a translation source key rendered through `t()`.
 */
export type OverviewModule = {
  id: OverviewModuleId
  label: string
  span: 1 | 2
}

export type OverviewModuleView = OverviewModule & { hidden: boolean }

export const OVERVIEW_LAYOUT_STORAGE_KEY = 'embypanel_overview_modules'

const defaultModules: OverviewModule[] = [
  { id: 'media', label: '媒体库总览', span: 1 },
  { id: 'health', label: '服务器状态', span: 1 },
  { id: 'operations', label: '运维概览', span: 2 },
  { id: 'rate-limit', label: '播放频率限制', span: 2 },
  { id: 'playing', label: '实时播放', span: 2 },
]

const defaultIds = defaultModules.map((module) => module.id)
const moduleById = new Map(defaultModules.map((module) => [module.id, module]))

function isModuleId(value: unknown): value is OverviewModuleId {
  return typeof value === 'string' && defaultIds.includes(value as OverviewModuleId)
}

function uniqueIds(value: unknown): OverviewModuleId[] {
  if (!Array.isArray(value)) return []
  const seen = new Set<OverviewModuleId>()
  const ids: OverviewModuleId[] = []
  value.forEach((entry) => {
    if (!isModuleId(entry) || seen.has(entry)) return
    seen.add(entry)
    ids.push(entry)
  })
  return ids
}

/** Accepts any stored shape; unknown, missing and duplicate ids fall back to the default order. */
function normalizeOrder(value: unknown): OverviewModuleId[] {
  const order = uniqueIds(value)
  const seen = new Set(order)
  defaultIds.forEach((id) => {
    if (!seen.has(id)) order.push(id)
  })
  return order
}

function defaultLayout() {
  return { order: [...defaultIds], hidden: [] as OverviewModuleId[] }
}

function loadLayout() {
  let raw = ''
  try {
    raw = window.localStorage.getItem(OVERVIEW_LAYOUT_STORAGE_KEY) ?? ''
  } catch {
    return defaultLayout()
  }
  if (!raw) return defaultLayout()

  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return defaultLayout()
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return defaultLayout()

  const record = parsed as Record<string, unknown>
  return { order: normalizeOrder(record.order), hidden: uniqueIds(record.hidden) }
}

/** Module metadata for a given order; unknown ids are skipped. */
function overviewModulesInOrder(order: OverviewModuleId[]): OverviewModule[] {
  return order
    .map((id) => moduleById.get(id))
    .filter((module): module is OverviewModule => Boolean(module))
}

export function useOverviewLayout() {
  const initial = loadLayout()

  // Applied state: what the dashboard actually renders and what is persisted.
  const appliedOrder = ref<OverviewModuleId[]>(initial.order)
  const appliedHidden = ref<OverviewModuleId[]>(initial.hidden)

  // Draft state: edited inside the settings dialog, only promoted on save.
  const draftOrder = ref<OverviewModuleId[]>([...initial.order])
  const draftHidden = ref<OverviewModuleId[]>([...initial.hidden])
  const editorOpen = ref(false)

  const modules = computed(() => overviewModulesInOrder(appliedOrder.value))
  const visibleModules = computed(() =>
    modules.value.filter((module) => !appliedHidden.value.includes(module.id)),
  )
  // Playback artwork fetching is expensive and page-level; it only runs while the
  // playing module is rendered as a real panel.
  const playingVisible = computed(() => !appliedHidden.value.includes('playing'))

  const draftModules = computed<OverviewModuleView[]>(() =>
    overviewModulesInOrder(draftOrder.value).map((module) => ({
      ...module,
      hidden: draftHidden.value.includes(module.id),
    })),
  )

  function persist() {
    try {
      window.localStorage.setItem(
        OVERVIEW_LAYOUT_STORAGE_KEY,
        JSON.stringify({ order: appliedOrder.value, hidden: appliedHidden.value }),
      )
    } catch {
      // Restricted storage keeps the current session layout in memory only.
    }
  }

  function openEditor() {
    draftOrder.value = [...appliedOrder.value]
    draftHidden.value = [...appliedHidden.value]
    editorOpen.value = true
  }

  /** Closes the editor and throws the draft away; the applied layout stays untouched. */
  function closeEditor() {
    draftOrder.value = [...appliedOrder.value]
    draftHidden.value = [...appliedHidden.value]
    editorOpen.value = false
  }

  /** Promotes the draft to the applied layout and persists it. */
  function applyDraft() {
    appliedOrder.value = [...draftOrder.value]
    appliedHidden.value = [...draftHidden.value]
    persist()
    editorOpen.value = false
  }

  /** Restores the default order and shows every module, but only inside the draft. */
  function resetDraft() {
    draftOrder.value = [...defaultIds]
    draftHidden.value = []
  }

  function toggleDraftModule(id: OverviewModuleId) {
    if (draftHidden.value.includes(id)) {
      draftHidden.value = draftHidden.value.filter((item) => item !== id)
      return
    }
    draftHidden.value = [...draftHidden.value, id]
  }

  function moveDraftToIndex(id: OverviewModuleId, targetIndex: number) {
    const from = draftOrder.value.indexOf(id)
    if (from < 0) return
    if (targetIndex < 0 || targetIndex >= draftOrder.value.length) return
    if (targetIndex === from) return
    const next = [...draftOrder.value]
    next.splice(from, 1)
    next.splice(targetIndex, 0, id)
    draftOrder.value = next
  }

  function moveDraftModule(id: OverviewModuleId, direction: -1 | 1) {
    moveDraftToIndex(id, draftOrder.value.indexOf(id) + direction)
  }

  function reorderDraft(sourceId: OverviewModuleId, targetId: OverviewModuleId) {
    if (sourceId === targetId) return
    moveDraftToIndex(sourceId, draftOrder.value.indexOf(targetId))
  }

  return {
    visibleModules,
    playingVisible,
    draftModules,
    editorOpen,
    openEditor,
    closeEditor,
    applyDraft,
    resetDraft,
    toggleDraftModule,
    moveDraftModule,
    reorderDraft,
  }
}
