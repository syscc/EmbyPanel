import { computed, ref } from 'vue'
import type { PlaybackSession, ServerOption } from '@/types/panel'
import type { Api, EncryptPayload } from '@/composables/useUsersController'

export type SessionDetail = PlaybackSession

type ConfirmAction = (options: Omit<import('@/composables/useActionDialog').ActionDialogRequest, 'kind'>) => Promise<boolean>
type PromptAction = (options: Omit<import('@/composables/useActionDialog').ActionDialogRequest, 'kind'>) => Promise<string | null>
type Translate = (source: string) => string
type ShowNotice = (message: string) => void
type ClearNotice = () => void

function sessionActionKey(sessionId: string, serverId: string) {
  return `${serverId}\u0000${sessionId}`
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}

function isAbortError(error: unknown) {
  return error instanceof DOMException && error.name === 'AbortError'
}

export function useSessionsController(options: {
  api: Api
  servers: () => ServerOption[]
  canControlSessions: () => boolean
  encryptPayload: EncryptPayload
  t: Translate
  confirmAction: ConfirmAction
  promptAction: PromptAction
  showNotice: ShowNotice
  clearNotice?: ClearNotice
}) {
  const sessions = ref<SessionDetail[]>([])
  const selectedServer = ref('all')
  const loading = ref(false)
  const error = ref('')
  const partialError = ref('')
  const actionSessionId = ref<string | null>(null)

  let disposed = false
  let refreshSeq = 0
  let abortController: AbortController | null = null
  let actionController: AbortController | null = null

  const filteredSessions = computed(() => {
    if (selectedServer.value === 'all') return sessions.value
    return sessions.value.filter((session) => session.server_id === selectedServer.value)
  })

  const serverOptions = computed(() => [
    { value: 'all', label: options.t('全部服务器') },
    ...options.servers().map((server) => ({ value: server.id, label: server.name || server.id })),
  ])

  const refreshSessions = async () => {
    if (disposed || loading.value) return
    const seq = ++refreshSeq
    abortController?.abort()
    const controller = new AbortController()
    abortController = controller
    loading.value = true
    error.value = ''
    partialError.value = ''
    options.clearNotice?.()

    try {
      const data = await options.api<SessionDetail[]>('/api/monitoring/plays', {
        signal: controller.signal,
        onResponseHeaders: (headers) => {
          if (disposed || seq !== refreshSeq) return
          const warning = headers.get('x-embypanel-server-errors')
          if (!warning) return
          partialError.value = options.t('部分服务器会话查询失败，当前列表不完整')
          try {
            const metadata = JSON.parse(warning)
            if (Array.isArray(metadata.server_ids)) {
              partialError.value += ': ' + metadata.server_ids.filter((id: unknown) => typeof id === 'string').join(', ')
            }
          } catch {
            // A warning must remain visible even if its metadata is malformed.
          }
        },
      })
      if (disposed || seq !== refreshSeq) return
      sessions.value = data
      if (selectedServer.value !== 'all' && !options.servers().some((server) => server.id === selectedServer.value)) {
        selectedServer.value = 'all'
      }
    } catch (err: unknown) {
      if (disposed || seq !== refreshSeq || controller.signal.aborted || isAbortError(err)) return
      error.value = errorMessage(err)
    } finally {
      if (!disposed && seq === refreshSeq) {
        loading.value = false
        if (abortController === controller) abortController = null
      }
    }
  }

  const stopPlayback = async (sessionId: string, serverId: string) => {
    if (disposed || actionSessionId.value || !options.canControlSessions()) return
    const confirmed = await options.confirmAction({
      title: options.t('停止播放'),
      description: options.t('确定停止此播放会话？'),
      confirmText: options.t('停止'),
      cancelText: options.t('取消'),
      tone: 'warning',
    })
    if (!confirmed || disposed || !options.canControlSessions()) return

    const controller = new AbortController()
    actionController = controller
    actionSessionId.value = sessionActionKey(sessionId, serverId)
    try {
      const body = await options.encryptPayload('stop_session', { server_id: serverId })
      if (disposed || controller.signal.aborted || !options.canControlSessions()) return
      await options.api(`/api/sessions/${encodeURIComponent(sessionId)}/stop`, {
        method: 'POST',
        body: JSON.stringify(body),
        signal: controller.signal,
      })
      if (disposed || controller.signal.aborted) return
      await refreshSessions()
      if (!disposed && !controller.signal.aborted) options.showNotice(options.t('播放会话已停止'))
    } catch (err: unknown) {
      if (disposed || controller.signal.aborted || isAbortError(err)) return
      options.showNotice(`${options.t('停止播放失败')}: ${errorMessage(err)}`)
    } finally {
      if (actionController === controller) actionController = null
      if (!disposed) actionSessionId.value = null
    }
  }

  const sendMessage = async (sessionId: string, serverId: string) => {
    if (disposed || actionSessionId.value || !options.canControlSessions()) return
    const message = (await options.promptAction({
      title: options.t('发送消息'),
      description: options.t('请输入消息内容'),
      inputLabel: options.t('消息内容'),
      inputType: 'text',
      inputPlaceholder: options.t('输入您的消息'),
    }))?.trim()
    if (!message) {
      if (message !== null && message !== undefined) options.showNotice(options.t('消息不能为空'))
      return
    }
    if (disposed || !options.canControlSessions()) return

    const controller = new AbortController()
    actionController = controller
    actionSessionId.value = sessionActionKey(sessionId, serverId)
    try {
      const body = await options.encryptPayload('send_session_message', { server_id: serverId, message })
      if (disposed || controller.signal.aborted || !options.canControlSessions()) return
      await options.api(`/api/sessions/${encodeURIComponent(sessionId)}/message`, {
        method: 'POST',
        body: JSON.stringify(body),
        signal: controller.signal,
      })
      if (disposed || controller.signal.aborted) return
      await refreshSessions()
      if (!disposed && !controller.signal.aborted) options.showNotice(options.t('消息已发送'))
    } catch (err: unknown) {
      if (disposed || controller.signal.aborted || isAbortError(err)) return
      options.showNotice(`${options.t('发送消息失败')}: ${errorMessage(err)}`)
    } finally {
      if (actionController === controller) actionController = null
      if (!disposed) actionSessionId.value = null
    }
  }

  let refreshTimer: ReturnType<typeof setInterval> | undefined

  function refreshVisible() {
    if (!document.hidden) void refreshSessions()
    else {
      refreshSeq += 1
      abortController?.abort()
      abortController = null
      loading.value = false
    }
  }

  function startAutoRefresh() {
    if (disposed || refreshTimer !== undefined) return
    refreshTimer = setInterval(refreshVisible, 10000)
    document.addEventListener('visibilitychange', refreshVisible)
    refreshVisible()
  }

  function dispose() {
    if (refreshTimer !== undefined) clearInterval(refreshTimer)
    document.removeEventListener('visibilitychange', refreshVisible)
    disposed = true
    refreshSeq += 1
    abortController?.abort()
    abortController = null
    actionController?.abort()
    actionController = null
  }

  return {
    sessions,
    selectedServer,
    loading,
    error,
    partialError,
    actionSessionId,
    serverOptions,
    servers: serverOptions,
    filteredSessions,
    refresh: refreshSessions,
    stopPlayback,
    sendMessage,
    refreshSessions,
    startAutoRefresh,
    dispose,
  }
}

export type SessionsController = ReturnType<typeof useSessionsController>
