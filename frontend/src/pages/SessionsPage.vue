<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { MessageCircle, RefreshCw, StopCircle } from '@lucide/vue'
import { usePanelContext } from '@/composables/panel-context'
import { useSessionsController, type SessionDetail } from '@/composables/useSessionsController'
import { formatTicks } from '@/lib/panel-formatters'
import SettingsDialogShell from '@/components/ui/SettingsDialogShell.vue'

const panel = usePanelContext()
const { api, encryptPayload, t, showNotice, clearNotice, confirmAction, promptAction } = panel
const { canControlSessions } = panel
const controller = useSessionsController({
  servers: () => panel.serverOptions.value,
  canControlSessions: () => canControlSessions.value,
  api,
  encryptPayload,
  t,
  confirmAction,
  promptAction,
  showNotice,
  clearNotice,
})
const {
  sessions,
  selectedServer,
  loading,
  error,
  partialError,
  serverOptions,
  filteredSessions,
  actionSessionId,
  refresh,
  stopPlayback,
  sendMessage,
  startAutoRefresh,
  dispose,
} = controller

const selectedSession = ref<SessionDetail | null>(null)
const dialogOpen = ref(false)
const refreshing = ref(false)
let refreshGeneration = 0
const selectedCountLabel = computed(() => `${filteredSessions.value.length} ${t('个活跃会话')}`)

async function refreshPage() {
  if (refreshing.value || loading.value) return
  const generation = ++refreshGeneration
  refreshing.value = true
  try {
    if (!await panel.refreshServerOptions()) return
    await refresh()
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    if (generation === refreshGeneration) refreshing.value = false
  }
}

function showDetail(session: SessionDetail) {
  selectedSession.value = session
  dialogOpen.value = true
}

function onDetailOpenChange(open: boolean) {
  dialogOpen.value = open
  if (!open) selectedSession.value = null
}

function playbackModeLabel(mode: SessionDetail['playback_mode']) {
  const labels: Record<SessionDetail['playback_mode'], string> = {
    direct_link: t('直链播放'),
    server_proxy: t('服务器代理播放'),
    transcode: t('服务器转码'),
    emby_direct_play: t('Emby 直接播放'),
    emby_direct_stream: t('Emby 直接串流'),
    unknown: t('播放路径待确认'),
  }
  return labels[mode] || t('播放路径待确认')
}

function sessionActionKey(session: SessionDetail) {
  return `${session.server_id}\u0000${session.id}`
}

function isActionPending(session: SessionDetail) {
  return actionSessionId.value === sessionActionKey(session)
}

function sessionTitle(session: SessionDetail) {
  const itemName = sessionValue(session.item_name)
  return session.series_name?.trim() ? `${session.series_name} · ${itemName}` : itemName
}

function sessionValue(value: string | number | null | undefined) {
  if (value === null || value === undefined || (typeof value === 'string' && !value.trim())) {
    return t('暂无详情')
  }
  return String(value)
}

function transcodeReasonLabel(session: SessionDetail) {
  return sessionValue(session.transcode_reason)
}

onMounted(() => {
  startAutoRefresh()
})

onUnmounted(() => {
  dispose()
})
</script>

<template>
  <div class="p-6">
    <div v-if="error" class="bg-red-100 border border-red-400 text-red-700 px-4 py-3 rounded mb-4">
      {{ error }}
    </div>

    <div v-if="partialError" role="status" class="bg-amber-100 text-amber-800 px-4 py-3 rounded mb-4">{{ partialError }}</div>

    <div class="flex justify-between items-center mb-6">
      <div class="flex items-center gap-4">
        <select v-model="selectedServer" class="border p-2 rounded" :aria-label="t('服务器')">
          <option v-for="option in serverOptions" :key="option.value" :value="option.value">
            {{ option.label }}
          </option>
        </select>
        <button
          type="button"
          :disabled="loading || refreshing"
          class="secondary icon-button shrink-0 disabled:cursor-not-allowed disabled:opacity-50"
          :aria-label="t('刷新')"
          :title="t('刷新')"
          @click="refreshPage"
        >
          <RefreshCw class="h-4 w-4" :class="{ 'animate-spin': loading }" aria-hidden="true" />
        </button>
      </div>
      <div class="text-sm text-gray-500">{{ selectedCountLabel }}</div>
    </div>

    <div v-if="loading" class="text-center py-12">{{ t('加载中...') }}</div>
    <div v-else-if="!error && sessions.length === 0" class="text-center py-12 text-gray-500">
      {{ t('暂无活跃会话') }}
    </div>
    <div v-else-if="!error && filteredSessions.length === 0" class="text-center py-12 text-gray-500">
      {{ t('暂无活跃会话') }}
    </div>
    <div v-else class="overflow-x-auto">
      <table class="min-w-full divide-y divide-gray-200">
        <thead>
          <tr>
            <th class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">{{ t('服务器') }}</th>
            <th class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">{{ t('用户') }}</th>
            <th class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">{{ t('播放信息') }}</th>
            <th class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">{{ t('设备') }}</th>
            <th class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">{{ t('进度') }}</th>
            <th class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">{{ t('播放信息') }}</th>
            <th class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider">{{ t('操作') }}</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-gray-200">
          <tr v-for="session in filteredSessions" :key="`${session.server_id}-${session.id}`">
            <td class="px-6 py-4 whitespace-nowrap">{{ sessionValue(session.server_name) }}</td>
            <td class="px-6 py-4">{{ sessionValue(session.user_name) }}</td>
            <td class="px-6 py-4">{{ sessionTitle(session) }}</td>
            <td class="px-6 py-4">
              {{ sessionValue(session.device_name) }}
              <span class="text-xs text-gray-400 ml-2">({{ sessionValue(session.client) }})</span>
              <span v-if="session.transcode_reason" class="text-xs text-amber-600 ml-1">({{ transcodeReasonLabel(session) }})</span>
              <span v-if="session.application_version" class="text-xs text-gray-400 ml-1">v{{ session.application_version }}</span>
            </td>
            <td class="px-6 py-4 whitespace-nowrap">
              <span v-if="session.percent !== null && session.percent !== undefined">
                {{ session.percent }}%
                <span class="text-xs text-gray-500">({{ formatTicks(session.position_ticks) }} / {{ formatTicks(session.runtime_ticks) }})</span>
              </span>
              <span v-else>{{ t('暂无详情') }}</span>
            </td>
            <td class="px-6 py-4">{{ playbackModeLabel(session.playback_mode) }}</td>
            <td class="px-6 py-4 flex gap-2">
              <button
                type="button"
                v-if="canControlSessions"
                class="text-red-600 hover:text-red-900 disabled:opacity-50"
                :disabled="isActionPending(session)"
                :aria-label="t('停止播放')"
                @click="stopPlayback(session.id, session.server_id)"
              >
                <StopCircle class="w-5 h-5" />
              </button>
              <button
                type="button"
                v-if="canControlSessions"
                class="text-blue-600 hover:text-blue-900 disabled:opacity-50"
                :disabled="isActionPending(session)"
                :aria-label="t('发送消息')"
                @click="sendMessage(session.id, session.server_id)"
              >
                <MessageCircle class="w-5 h-5" />
              </button>
              <button
                type="button"
                class="text-gray-600 hover:text-gray-900 disabled:opacity-50"
                :disabled="isActionPending(session)"
                :aria-label="t('查看详情')"
                @click="showDetail(session)"
              >
                {{ t('查看详情') }}
              </button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <SettingsDialogShell
      :open="dialogOpen"
      :title="selectedSession ? sessionTitle(selectedSession) : t('会话详情')"
      :description="t('会话详细信息')"
      :close-label="t('关闭')"
      @update:open="onDetailOpenChange"
    >
      <dl v-if="selectedSession" class="grid gap-3 p-4 sm:grid-cols-2">
        <div><dt class="font-semibold">{{ t('服务器') }}</dt><dd>{{ sessionValue(selectedSession.server_name) }}</dd></div>
        <div><dt class="font-semibold">{{ t('用户') }}</dt><dd>{{ sessionValue(selectedSession.user_name) }}</dd></div>
        <div><dt class="font-semibold">{{ t('设备') }}</dt><dd>{{ sessionValue(selectedSession.device_name) }} · {{ sessionValue(selectedSession.client) }}</dd></div>
        <div><dt class="font-semibold">{{ t('用户代理') }}</dt><dd class="break-words">{{ sessionValue(selectedSession.user_agent) }}</dd></div>
        <div><dt class="font-semibold">{{ t('应用版本') }}</dt><dd>{{ sessionValue(selectedSession.application_version) }}</dd></div>
        <div><dt class="font-semibold">{{ t('设备 ID') }}</dt><dd class="break-words">{{ sessionValue(selectedSession.device_id) }}</dd></div>
        <div><dt class="font-semibold">{{ t('远程端点') }}</dt><dd class="break-words">{{ sessionValue(selectedSession.remote_endpoint) }}</dd></div>
        <div><dt class="font-semibold">{{ t('媒体 ID') }}</dt><dd>{{ sessionValue(selectedSession.item_id) }}</dd></div>
        <div><dt class="font-semibold">{{ t('播放信息') }}</dt><dd>{{ playbackModeLabel(selectedSession.playback_mode) }}</dd></div>
        <div><dt class="font-semibold">{{ t('码率') }}</dt><dd>{{ sessionValue(selectedSession.bitrate) }}</dd></div>
        <div><dt class="font-semibold">{{ t('带宽') }}</dt><dd>{{ sessionValue(selectedSession.bandwidth) }}</dd></div>
        <div><dt class="font-semibold">{{ t('转码原因') }}</dt><dd class="break-words">{{ transcodeReasonLabel(selectedSession) }}</dd></div>
        <div><dt class="font-semibold">{{ t('进度') }}</dt><dd>{{ formatTicks(selectedSession.position_ticks) }} / {{ formatTicks(selectedSession.runtime_ticks) }}</dd></div>
        <div><dt class="font-semibold">{{ t('来源') }}</dt><dd>{{ sessionValue(selectedSession.playback_ip) }}</dd></div>
      </dl>
    </SettingsDialogShell>
  </div>
</template>
