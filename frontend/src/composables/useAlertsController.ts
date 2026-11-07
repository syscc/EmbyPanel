import { ref, type Ref } from 'vue'
import { usePayloadEncryption } from '@/composables/usePayloadEncryption'
import type { ActionDialogRequest } from '@/composables/useActionDialog'

type Api = <T>(path: string, init?: RequestInit) => Promise<T>
type Translate = (source: string) => string
type ShowNotice = (message: string) => void
type ActionDialogOptions = Omit<ActionDialogRequest, 'kind'>
type ConfirmAction = (options: ActionDialogOptions) => Promise<boolean>
type PromptAction = (options: ActionDialogOptions) => Promise<string | null>

const MAX_EVALUATION_WINDOW_SECS = 7 * 24 * 60 * 60

/** Mirrors backend `AlertRule` (src/alerts.rs). */
export type AlertRule = {
  id: string
  name: string
  enabled: boolean
  rule_configured: boolean
  threshold: string
  silence_period: number
  created_at: number
  metric: string
  operator: string
  server_id: string
  evaluation_window_secs: number
}

export type CreateAlertRulePayload = {
  name: string
  enabled: boolean
  rule_config: string
  threshold: string
  silence_period: number
  metric: string
  operator: string
  server_id: string
  evaluation_window_secs: number
}

export type UpdateAlertRulePayload = {
  name: string
  enabled: boolean
  rule_config?: string
  clear_rule_config?: boolean
  threshold: string
  silence_period: number
  metric: string
  operator: string
  server_id: string
  evaluation_window_secs: number
}

export type AlertRuleDraft = {
  id: string
  name: string
  enabled: boolean
  rule_config: string
  rule_configured?: boolean
  threshold: string
  silence_period: number
  metric: string
  operator: string
  server_id: string
  evaluation_window_secs: number
  clear_rule_config?: boolean
}

/** Mirrors backend `AlertDelivery` (src/alerts.rs). */
export type AlertDelivery = {
  id: number
  rule_id: string
  timestamp_ms: number
  status: string
  retry_count: number
  message: string
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function emptyDraft(): AlertRuleDraft {
  return {
    id: '',
    name: '',
    enabled: true,
    rule_config: '',
    rule_configured: false,
    threshold: '',
    silence_period: 0,
    metric: '',
    operator: 'gte',
    server_id: '',
    evaluation_window_secs: 0,
    clear_rule_config: false,
  }
}

function draftFromRule(rule: AlertRule): AlertRuleDraft {
  return {
    id: rule.id,
    name: rule.name,
    enabled: rule.enabled,
    rule_config: '',
    rule_configured: rule.rule_configured,
    threshold: rule.threshold,
    silence_period: rule.silence_period,
    metric: rule.metric,
    operator: rule.operator,
    server_id: rule.server_id,
    evaluation_window_secs: rule.evaluation_window_secs,
    clear_rule_config: false,
  }
}

function toCreatePayload(draft: AlertRuleDraft): CreateAlertRulePayload {
  const p = toUpdatePayload(draft)
  return {
    ...p,
    rule_config: draft.rule_config.trim(),
  }
}
function toUpdatePayload(draft: AlertRuleDraft): UpdateAlertRulePayload {
  const p = {
    name: draft.name.trim(),
    enabled: draft.enabled,
    threshold: draft.threshold,
    silence_period: Math.floor(draft.silence_period) || 0,
    metric: draft.metric,
    operator: draft.operator,
    server_id: draft.server_id,
    evaluation_window_secs: draft.evaluation_window_secs,
  }
  return p
}

/**
 * Build silent `until` epoch-ms decimal string from hours in the future.
 * Backend accepts future epoch-ms; empty / "0" / "null" / "none" clears.
 */
export function silentUntilFromHours(
  hours: number,
  nowMs = Date.now(),
): string {
  if (!Number.isFinite(hours) || hours <= 0) return ''
  const until = Math.floor(nowMs + hours * 3_600_000)
  if (!Number.isSafeInteger(until) || until <= nowMs) return ''
  return String(until)
}

export function useAlertsController(options: {
  api: Api
  translate: Translate
  showNotice: ShowNotice
  confirmAction: ConfirmAction
  promptAction: PromptAction
}) {
  const rules: Ref<AlertRule[]> = ref([])
  const deliveries: Ref<AlertDelivery[]> = ref([])
  const historyByRule: Ref<Record<string, AlertDelivery[]>> = ref({})

  const loading = ref(false)
  const deliveriesLoading = ref(false)
  const saving = ref(false)
  const error = ref('')
  const deliveriesError = ref('')
  const historyError = ref('')
  const actionError = ref('')

  const dialogOpen = ref(false)
  const draft = ref<AlertRuleDraft>(emptyDraft())
  const historyRuleId = ref<string | null>(null)
  const historyOpen = ref(false)
  const historyLoading = ref(false)
  let rulesRequestVersion = 0
  let rulesAbortController: AbortController | null = null
  let historyRequestVersion = 0
  let deliveriesRequestVersion = 0
  let deliveriesAbortController: AbortController | null = null
  let disposed = false

  const togglingIds = ref<Record<string, boolean>>({})
  const testingIds = ref<Record<string, boolean>>({})
  const silencingIds = ref<Record<string, boolean>>({})
  const deletingIds = ref<Record<string, boolean>>({})

  const lifecycle = new AbortController()
  async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
    if (disposed) throw new DOMException('Disposed', 'AbortError')
    const result = await options.api<T>(path, {
      ...init,
      signal: init.signal ? AbortSignal.any([lifecycle.signal, init.signal]) : lifecycle.signal,
    })
    if (disposed) throw new DOMException('Disposed', 'AbortError')
    return result
  }

  const { encryptPayload } = usePayloadEncryption({
    api,
    translate: options.translate,
  })

  const clearRuleConfig = ref(false)

  const DEFAULT_TEST_PAYLOAD =
    '{"title":"EmbyPanel alert test","text":"Webhook test delivery"}'

  async function loadRules() {
    if (disposed) return
    const requestVersion = ++rulesRequestVersion
    rulesAbortController?.abort()
    const controller = new AbortController()
    rulesAbortController = controller
    loading.value = true
    error.value = ''
    try {
      const data = await api<AlertRule[]>('/api/alert-rules', { signal: controller.signal })
      if (disposed || requestVersion !== rulesRequestVersion) return
      rules.value = data.map((rule) => ({
        id: rule.id,
        name: rule.name,
        enabled: rule.enabled,
        rule_configured: rule.rule_configured,
        threshold: rule.threshold,
        silence_period: rule.silence_period,
        created_at: rule.created_at,
        metric: rule.metric,
        operator: rule.operator,
        server_id: rule.server_id,
        evaluation_window_secs: rule.evaluation_window_secs,
      }))
    } catch (cause) {
      if (disposed || requestVersion !== rulesRequestVersion || controller.signal.aborted) return
      error.value = errorMessage(cause)
    } finally {
      if (!disposed && requestVersion === rulesRequestVersion) {
        loading.value = false
        if (rulesAbortController === controller) rulesAbortController = null
      }
    }
  }

  async function loadDeliveries() {
    if (disposed) return
    const requestVersion = ++deliveriesRequestVersion
    deliveriesAbortController?.abort()
    const controller = new AbortController()
    deliveriesAbortController = controller
    deliveriesLoading.value = true
    deliveriesError.value = ''
    try {
      const data = await api<AlertDelivery[]>(
        '/api/alert-deliveries',
        { signal: controller.signal },
      )
      if (disposed || requestVersion !== deliveriesRequestVersion) return
      deliveries.value = data
    } catch (cause) {
      if (disposed) return
      if (
        disposed ||
        requestVersion !== deliveriesRequestVersion ||
        controller.signal.aborted
      ) {
        return
      }
      deliveriesError.value = errorMessage(cause)
    } finally {
      if (!disposed && requestVersion === deliveriesRequestVersion) {
        deliveriesLoading.value = false
        if (deliveriesAbortController === controller) {
          deliveriesAbortController = null
        }
      }
    }
  }

  async function loadRuleHistory(ruleId: string) {
    if (disposed) return
    const requestVersion = ++historyRequestVersion
    historyLoading.value = true
    historyError.value = ''
    try {
      const data = await api<AlertDelivery[]>(
        `/api/alert-rules/${encodeURIComponent(ruleId)}/history`,
      )
      if (
        disposed ||
        requestVersion !== historyRequestVersion ||
        !historyOpen.value ||
        historyRuleId.value !== ruleId
      ) {
        return
      }
      historyByRule.value[ruleId] = data
    } catch (cause) {
      if (disposed) return
      if (
        !disposed &&
        requestVersion === historyRequestVersion &&
        historyOpen.value &&
        historyRuleId.value === ruleId
      ) {
        historyError.value = errorMessage(cause)
      }
    } finally {
      if (!disposed && requestVersion === historyRequestVersion) {
        historyLoading.value = false
      }
    }
  }

  function openCreateDialog() {
    resetDraft()
    dialogOpen.value = true
  }

  function openEditDialog(rule: AlertRule) {
    draft.value = draftFromRule(rule)
    clearRuleConfig.value = false
    actionError.value = ''
    dialogOpen.value = true
  }

  function openRuleHistory(ruleId: string) {
    historyRuleId.value = ruleId
    historyOpen.value = true
    void loadRuleHistory(ruleId)
  }

  function setHistoryOpen(open: boolean) {
    if (open) {
      const ruleId = historyRuleId.value
      if (ruleId && !historyOpen.value) {
        historyOpen.value = true
        void loadRuleHistory(ruleId)
      }
      return
    }

    historyRequestVersion += 1
    historyOpen.value = false
    historyRuleId.value = null
    historyLoading.value = false
    historyError.value = ''
  }

  function resetDraft() {
    draft.value = emptyDraft()
    clearRuleConfig.value = false
    actionError.value = ''
  }

  function setDialogOpen(open: boolean) {
    dialogOpen.value = open
    if (!open) {
      resetDraft()
    }
  }

  function normalizedDraftForSubmit(
    source: AlertRuleDraft,
  ): AlertRuleDraft | null {
    const metric = source.metric.trim()
    const thresholdInput = String(source.threshold).trim()
    let threshold = thresholdInput
    if (metric) {
      const numericThreshold = Number(thresholdInput)
      if (!thresholdInput || !Number.isFinite(numericThreshold)) {
        actionError.value = options.translate('阈值必须是有限数值')
        return null
      }
      threshold = String(numericThreshold)
    }

    const evaluationWindowInput = String(source.evaluation_window_secs).trim()
    const evaluationWindow = Number(evaluationWindowInput)
    if (
      !evaluationWindowInput ||
      !Number.isFinite(evaluationWindow) ||
      !Number.isInteger(evaluationWindow) ||
      evaluationWindow < 0 ||
      evaluationWindow > MAX_EVALUATION_WINDOW_SECS
    ) {
      actionError.value = options.translate(
        '评估窗口必须是 0 到 604800 之间的整数',
      )
      return null
    }

    return {
      ...source,
      threshold,
      metric,
      operator: source.operator.trim() || 'gte',
      server_id: source.server_id.trim(),
      evaluation_window_secs: evaluationWindow,
    }
  }

  async function saveDraft() {
    if (disposed || saving.value) return
    actionError.value = ''
    const draftVal = normalizedDraftForSubmit({ ...draft.value })
    if (!draftVal) return

    saving.value = true
    const shouldClearRuleConfig = clearRuleConfig.value
    try {
      if (!draftVal.id) {
        const payload = toCreatePayload(draftVal)
        const encrypted = await encryptPayload('alert_rule', payload)
        if (disposed) return
        await api('/api/alert-rules', {
          method: 'POST',
          body: JSON.stringify(encrypted),
        })
        if (disposed) return
        options.showNotice(options.translate('规则已创建'))
        await loadRules()
      } else {
        let updatePayload: UpdateAlertRulePayload
        if (shouldClearRuleConfig) {
          updatePayload = {
            ...toUpdatePayload(draftVal),
            clear_rule_config: true,
          }
        } else if (draftVal.rule_config.trim()) {
          updatePayload = {
            ...toUpdatePayload(draftVal),
            rule_config: draftVal.rule_config.trim(),
          }
        } else {
          updatePayload = {
            ...toUpdatePayload(draftVal),
          }
        }
        const encrypted = await encryptPayload('alert_rule', updatePayload)
        if (disposed) return
        await api(
          `/api/alert-rules/${encodeURIComponent(draftVal.id)}`,
          {
            method: 'PUT',
            body: JSON.stringify(encrypted),
          },
        )
        if (disposed) return
        options.showNotice(options.translate('规则已更新'))
        await loadRules()
      }
      if (!disposed) setDialogOpen(false)
    } catch (cause) {
      if (disposed) return
      actionError.value = errorMessage(cause)
    } finally {
      if (!disposed) saving.value = false
    }
  }

  async function toggleRule(rule: AlertRule, enabled: boolean) {
    if (disposed || togglingIds.value[rule.id]) return
    actionError.value = ''
    togglingIds.value[rule.id] = true
    try {
      await api(
        `/api/alert-rules/${encodeURIComponent(rule.id)}/toggle`,
        {
          method: 'PATCH',
          body: JSON.stringify({ enabled }),
        },
      )
      if (disposed) return
      options.showNotice(
        enabled
          ? options.translate('规则已启用')
          : options.translate('规则已停用'),
      )
      await loadRules()
    } catch (cause) {
      if (disposed) return
      actionError.value = errorMessage(cause)
    } finally {
      if (!disposed) togglingIds.value[rule.id] = false
    }
  }

  async function deleteRule(rule: AlertRule) {
    if (disposed || deletingIds.value[rule.id]) return
    const confirmed = await options.confirmAction({
      title: options.translate('删除规则'),
      description: options.translate(
        '删除后规则及其交付历史都无法恢复，确定继续吗？',
      ),
      confirmText: options.translate('删除'),
      cancelText: options.translate('取消'),
      tone: 'danger',
    })
    if (!confirmed || disposed) return
    actionError.value = ''
    deletingIds.value[rule.id] = true
    try {
      await api(`/api/alert-rules/${encodeURIComponent(rule.id)}`, {
        method: 'DELETE',
      })
      if (disposed) return
      options.showNotice(options.translate('规则已删除'))
      await loadRules()
    } catch (cause) {
      if (disposed) return
      actionError.value = errorMessage(cause)
    } finally {
      if (!disposed) deletingIds.value[rule.id] = false
    }
  }

  async function testRule(rule: AlertRule) {
    if (disposed || testingIds.value[rule.id]) return
    actionError.value = ''
    testingIds.value[rule.id] = true
    try {
      await api(
        `/api/alert-rules/${encodeURIComponent(rule.id)}/test`,
        {
          method: 'POST',
        },
      )
      if (disposed) return
      actionError.value = ''
      options.showNotice(options.translate('消息已发送'))
      if (!disposed) {
        await loadDeliveries()
      }
    } catch (cause) {
      if (disposed) return
      actionError.value = errorMessage(cause)
    } finally {
      if (!disposed) testingIds.value[rule.id] = false
    }
  }

  async function promptSilence(rule: AlertRule) {
    if (disposed || silencingIds.value[rule.id]) return
    const hoursStr = await options.promptAction({
      title: options.translate('设置静默'),
      description: options.translate('请输入静默时长（小时）'),
      inputLabel: options.translate('静默时长（小时）'),
      inputType: 'text',
      confirmText: options.translate('设置'),
      cancelText: options.translate('取消'),
    })
    if (hoursStr === null || disposed) return
    const hoursInput = hoursStr.trim()
    if (!hoursInput) {
      actionError.value = options.translate('请输入有效的静默时长')
      return
    }
    const hours = Number(hoursInput)
    if (!Number.isFinite(hours) || hours <= 0) {
      actionError.value = options.translate('请输入有效的静默时长')
      return
    }
    const until = silentUntilFromHours(hours)
    if (!until) {
      actionError.value = options.translate('请输入有效的静默时长')
      return
    }
    actionError.value = ''
    silencingIds.value[rule.id] = true
    try {
      await api(
        `/api/alert-rules/${encodeURIComponent(rule.id)}/silent`,
        {
          method: 'PATCH',
          body: JSON.stringify({ until }),
        },
      )
      if (disposed) return
      options.showNotice(options.translate('静默已设置'))
      await loadRules()
    } catch (cause) {
      if (disposed) return
      actionError.value = errorMessage(cause)
    } finally {
      if (!disposed) silencingIds.value[rule.id] = false
    }
  }

  async function clearSilence(rule: AlertRule) {
    if (disposed || silencingIds.value[rule.id]) return
    actionError.value = ''
    silencingIds.value[rule.id] = true
    try {
      await api(
        `/api/alert-rules/${encodeURIComponent(rule.id)}/silent`,
        {
          method: 'PATCH',
          body: JSON.stringify({ until: '0' }),
        },
      )
      if (disposed) return
      options.showNotice(options.translate('静默已取消'))
      await loadRules()
    } catch (cause) {
      if (disposed) return
      actionError.value = errorMessage(cause)
    } finally {
      if (!disposed) silencingIds.value[rule.id] = false
    }
  }

  async function refreshAll() {
    await Promise.all([loadRules(), loadDeliveries()])
  }

  function dispose() {
    disposed = true
    lifecycle.abort()
    rulesRequestVersion += 1
    rulesAbortController?.abort()
    rulesAbortController = null
    historyRequestVersion += 1
    deliveriesRequestVersion += 1
    deliveriesAbortController?.abort()
    deliveriesAbortController = null
  }

  return {
    rules,
    deliveries,
    historyByRule,
    loading,
    deliveriesLoading,
    saving,
    error,
    deliveriesError,
    historyError,
    actionError,
    dialogOpen,
    draft,
    historyRuleId,
    historyOpen,
    historyLoading,
    togglingIds,
    testingIds,
    silencingIds,
    deletingIds,
    clearRuleConfig,
    loadRules,
    loadDeliveries,
    openRuleHistory,
    openCreateDialog,
    openEditDialog,
    setDialogOpen,
    setHistoryOpen,
    saveDraft,
    toggleRule,
    deleteRule,
    testRule,
    promptSilence,
    clearSilence,
    refreshAll,
    dispose,
  }
}
