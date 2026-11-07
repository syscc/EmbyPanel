<script setup lang="ts">
import { computed, ref, type Component } from 'vue'
import {
  Check,
  ChevronRight,
  Clock3,
  Database,
  Eye,
  EyeOff,
  Globe2,
  HeartPulse,
  KeyRound,
  Link2,
  ListFilter,
  LoaderCircle,
  Plus,
  Settings2,
  RefreshCw,
  Route,
  RotateCw,
  Server,
  ShieldCheck,
  Timer,
  Trash2,
  Waypoints,
  Zap,
} from '@lucide/vue'
import { DialogClose } from 'reka-ui'
import CheckboxField from '@/components/ui/CheckboxField.vue'
import SettingsDialogShell from '@/components/ui/SettingsDialogShell.vue'
import UiSwitch from '@/components/ui/UiSwitch.vue'
import { usePanelContext } from '@/composables/panel-context'
import type { EmbyServerConfig } from '@/types/panel'

const {
  settings,
  saving,
  error,
  restartingServerId,
  validationResults,
  defaultCdnHeaders,
  realIpModeOptions,
  proxyStatusById,
  strmMappingPlaceholder,
  t,
  confirmAction,
  addServer,
  validateSettings,
  saveSettings,
  toggleProxyServer,
  restartProxyServer,
  removeServer,
  proxyStatusLabel,
  formatTimestampMs,
  isApiKeyVisible,
  isApiKeyRevealLoading,
  apiKeyInputValue,
  apiKeyPlaceholder,
  updateApiKeyInput,
  toggleApiKeyVisible,
  needsRealIpHeader,
  updateRealIpMode,
  validationClass,
  localizeValidationText,
} = usePanelContext()

const removingServerId = ref('')

type SettingEditorKey =
  | 'cache_domain_filter_mode'
  | 'cache_domain_whitelist'
  | 'enable_internal_redirect'
  | 'internal_redirect_timeout_seconds'
  | 'strm_url_mappings'
  | 'cache_ttl_seconds'
  | 'cache_max_capacity'
  | 'openlist_addr'
  | 'openlist_token'
  | 'connectivity_check_enabled'
  | 'connectivity_check_interval_seconds'
  | 'connectivity_check_timeout_seconds'
  | 'connectivity_auto_restart_seconds'

type SettingEditorType =
  | 'boolean'
  | 'number'
  | 'password'
  | 'select'
  | 'text'
  | 'textarea'

type SettingToggleKey =
  | 'cache_enabled'
  | 'strm_url_mapping_enabled'
  | 'enable_internal_redirect'
  | 'connectivity_check_enabled'

type SettingCard = {
  key: SettingEditorKey
  label: string
  icon: Component
  editor: SettingEditorType
  toggleKey?: SettingToggleKey
  min?: number
  max?: number
  help?: string
  options?: Array<{ value: string; label: string }>
}

const routingSettingCards: SettingCard[] = [
  {
    key: 'cache_domain_filter_mode',
    label: '缓存过滤模式',
    icon: ListFilter,
    editor: 'select',
    options: [
      { value: 'off', label: '不过滤' },
      { value: 'whitelist', label: '白名单：命中才缓存' },
      { value: 'blacklist', label: '黑名单：命中不缓存' },
    ],
  },
  {
    key: 'cache_domain_whitelist',
    label: '缓存过滤域名',
    icon: Globe2,
    editor: 'textarea',
    help: '只匹配直链域名部分。白名单命中才缓存；黑名单命中不缓存，其他直链正常缓存。',
  },
  {
    key: 'enable_internal_redirect',
    label: '开启内部重定向 HEAD 解析',
    icon: Route,
    editor: 'boolean',
    toggleKey: 'enable_internal_redirect',
  },
  {
    key: 'internal_redirect_timeout_seconds',
    label: 'HEAD 超时秒数',
    icon: Timer,
    editor: 'number',
    min: 1,
  },
  {
    key: 'strm_url_mappings',
    label: 'STRM URL 映射',
    icon: Waypoints,
    editor: 'textarea',
    toggleKey: 'strm_url_mapping_enabled',
  },
]

const cacheSettingCards: SettingCard[] = [
  {
    key: 'cache_ttl_seconds',
    label: '缓存秒数',
    icon: Clock3,
    editor: 'number',
    toggleKey: 'cache_enabled',
    min: 0,
  },
  {
    key: 'cache_max_capacity',
    label: '缓存最大条数',
    icon: Database,
    editor: 'number',
    toggleKey: 'cache_enabled',
    min: 1,
  },
  {
    key: 'openlist_addr',
    label: 'OpenList 地址',
    icon: Link2,
    editor: 'text',
  },
  {
    key: 'openlist_token',
    label: 'OpenList Token',
    icon: KeyRound,
    editor: 'password',
  },
  {
    key: 'connectivity_check_enabled',
    label: '启用服务器连通性巡检',
    icon: HeartPulse,
    editor: 'boolean',
    toggleKey: 'connectivity_check_enabled',
  },
  {
    key: 'connectivity_check_interval_seconds',
    label: '巡检间隔秒数',
    icon: RefreshCw,
    editor: 'number',
    min: 10,
    max: 3600,
  },
  {
    key: 'connectivity_check_timeout_seconds',
    label: '单项超时秒数',
    icon: Timer,
    editor: 'number',
    min: 1,
    max: 60,
  },
  {
    key: 'connectivity_auto_restart_seconds',
    label: '反代无响应自动重启秒数',
    icon: RotateCw,
    editor: 'number',
    min: 0,
    max: 86400,
    help: '填 0 表示不自动重启；只在反代端口连续无响应时触发。',
  },
]

const allSettingCards = [...routingSettingCards, ...cacheSettingCards]
const settingEditorOpen = ref(false)
const activeSettingKey = ref<SettingEditorKey | null>(null)
const editorTextDraft = ref('')
const editorNumberDraft = ref(0)
const serverEditorOpen = ref(false)
const serverDraft = ref<EmbyServerConfig | null>(null)
const serverEditorIsNew = ref(false)
const serverEditorError = ref('')
const validationDialogOpen = ref(false)
const validationRunning = ref(false)

const activeSettingCard = computed(
  () => allSettingCards.find((card) => card.key === activeSettingKey.value) ?? null,
)

function configuredLineCount(value: string) {
  return value.split(/\r?\n/).filter((line) => line.trim()).length
}

function isSettingCardDisabled(card: SettingCard) {
  return (
    card.key === 'cache_domain_whitelist' &&
    settings.cache_domain_filter_mode === 'off'
  )
}

function settingSummary(card: SettingCard) {
  switch (card.key) {
    case 'cache_domain_filter_mode':
      return t(
        settings.cache_domain_filter_mode === 'whitelist'
          ? '白名单：命中才缓存'
          : settings.cache_domain_filter_mode === 'blacklist'
            ? '黑名单：命中不缓存'
            : '不过滤',
      )
    case 'cache_domain_whitelist':
    case 'strm_url_mappings': {
      const count = configuredLineCount(settings[card.key])
      return count ? `${count} ${t('条')}` : t('未配置')
    }
    case 'enable_internal_redirect':
    case 'connectivity_check_enabled':
      return t(settings[card.key] ? '已开启' : '已关闭')
    case 'cache_max_capacity':
      return `${settings.cache_max_capacity} ${t('条')}`
    case 'openlist_addr':
      return settings.openlist_addr?.trim() || t('未配置')
    case 'openlist_token':
      // Saved tokens are redacted; a persisted OpenList address implies its paired token exists.
      return settings.openlist_token?.trim() || settings.openlist_addr?.trim()
        ? t('已配置')
        : t('未配置')
    default:
      return `${settings[card.key]} ${t('秒')}`
  }
}

function settingPlaceholder(card: SettingCard) {
  switch (card.key) {
    case 'cache_domain_whitelist':
      return t('支持多个域名、通配符或关键字；每行一个，例如：*.115cdn.* 或 115')
    case 'strm_url_mappings':
      return strmMappingPlaceholder.value
    case 'openlist_addr':
      return `${t('可选')}：http://openlist.local:5244`
    case 'openlist_token':
      return t('可选')
    default:
      return undefined
  }
}

function openSettingEditor(card: SettingCard) {
  if (isSettingCardDisabled(card) || card.editor === 'boolean') return
  activeSettingKey.value = card.key
  const currentValue = settings[card.key]
  if (card.editor === 'number') {
    editorNumberDraft.value = Number(currentValue)
  } else {
    editorTextDraft.value = currentValue == null ? '' : String(currentValue)
  }
  settingEditorOpen.value = true
}

function settingBooleanValue(card: SettingCard) {
  return card.toggleKey ? settings[card.toggleKey] : false
}

async function toggleBooleanSetting(card: SettingCard) {
  if (saving.value || !card.toggleKey) return
  settings[card.toggleKey] = !settings[card.toggleKey]
  await saveSettings()
}

function handleSettingCardClick(card: SettingCard) {
  if (saving.value) return
  if (isSettingCardDisabled(card)) return
  openSettingEditor(card)
}

async function applySettingEditor() {
  if (saving.value) return
  switch (activeSettingKey.value) {
    case 'cache_domain_filter_mode':
      if (
        editorTextDraft.value === 'off' ||
        editorTextDraft.value === 'whitelist' ||
        editorTextDraft.value === 'blacklist'
      ) {
        settings.cache_domain_filter_mode = editorTextDraft.value
      }
      break
    case 'cache_domain_whitelist':
      settings.cache_domain_whitelist = editorTextDraft.value
      break
    case 'internal_redirect_timeout_seconds':
      settings.internal_redirect_timeout_seconds = editorNumberDraft.value
      break
    case 'strm_url_mappings':
      settings.strm_url_mappings = editorTextDraft.value
      break
    case 'cache_ttl_seconds':
      settings.cache_ttl_seconds = editorNumberDraft.value
      break
    case 'cache_max_capacity':
      settings.cache_max_capacity = editorNumberDraft.value
      break
    case 'openlist_addr':
      settings.openlist_addr = editorTextDraft.value
      break
    case 'openlist_token':
      settings.openlist_token = editorTextDraft.value
      break
    case 'connectivity_check_interval_seconds':
      settings.connectivity_check_interval_seconds = editorNumberDraft.value
      break
    case 'connectivity_check_timeout_seconds':
      settings.connectivity_check_timeout_seconds = editorNumberDraft.value
      break
    case 'connectivity_auto_restart_seconds':
      settings.connectivity_auto_restart_seconds = editorNumberDraft.value
      break
  }
  settingEditorOpen.value = false
  await saveSettings()
}

function openServerEditor(server: EmbyServerConfig) {
  openServerEditorFor(server, false)
}

function openServerEditorFor(server: EmbyServerConfig, isNew: boolean) {
  serverDraft.value = { ...server }
  serverEditorIsNew.value = isNew
  serverEditorError.value = ''
  serverEditorOpen.value = true
}

function addServerAndEdit() {
  if (saving.value) return
  addServer()
  const server = settings.servers.at(-1)
  if (server) openServerEditorFor(server, true)
}

function handleServerEditorOpenChange(open: boolean) {
  if (!open && serverEditorIsNew.value && serverDraft.value) {
    settings.servers = settings.servers.filter(
      (server) => server.id !== serverDraft.value?.id,
    )
  }
  if (!open) {
    serverDraft.value = null
    serverEditorIsNew.value = false
    serverEditorError.value = ''
  }
  serverEditorOpen.value = open
}

function validateServerDraft(draft: EmbyServerConfig) {
  if (!draft.name.trim()) return t('服务器名称不能为空')
  if (!draft.emby_host.trim()) return t('请填写 Emby 地址')
  if (serverEditorIsNew.value && !draft.emby_api_key.trim()) {
    return t('新增服务器必须填写 Emby API Key')
  }
  const port = Number(draft.port)
  if (!Number.isInteger(port) || port < 1 || port > 65535 || port === 8090) {
    return t('反代端口无效或被管理端口占用')
  }

  const globalOpenlistConfigured = Boolean(settings.openlist_addr?.trim())
  const effectiveOpenlistAddress =
    draft.openlist_addr_override == null
      ? globalOpenlistConfigured
      : Boolean(draft.openlist_addr_override.trim())
  const effectiveOpenlistToken = Boolean(
    draft.openlist_token_override_configured ||
    draft.openlist_token_override?.trim() ||
    globalOpenlistConfigured,
  )
  if (effectiveOpenlistAddress !== effectiveOpenlistToken) {
    return t('OpenList 地址和 Token 必须按最终有效配置成对存在')
  }
  return ''
}

type StringServerOverrideKey =
  | 'openlist_addr_override'
  | 'openlist_token_override'
  | 'strm_url_mappings_override'
  | 'cache_domain_whitelist_override'

type NumberServerOverrideKey =
  | 'cache_ttl_seconds_override'
  | 'cache_max_capacity_override'
  | 'internal_redirect_timeout_seconds_override'
  | 'connectivity_check_interval_seconds_override'
  | 'connectivity_check_timeout_seconds_override'
  | 'connectivity_auto_restart_seconds_override'

type BooleanServerOverrideKey =
  | 'strm_url_mapping_enabled_override'
  | 'cache_enabled_override'
  | 'enable_internal_redirect_override'
  | 'connectivity_check_enabled_override'

type SelectServerOverrideKey = 'cache_domain_filter_mode_override'
type ServerPolicyOverrideKey =
  | StringServerOverrideKey
  | NumberServerOverrideKey
  | BooleanServerOverrideKey
  | SelectServerOverrideKey

function serverOverrideIsSet(key: ServerPolicyOverrideKey) {
  const draft = serverDraft.value
  if (!draft) return false
  if (key === 'openlist_token_override' && draft.openlist_token_override_configured) return true
  return draft[key] !== null && draft[key] !== undefined
}

function serverOverrideStatus(key: ServerPolicyOverrideKey) {
  return t(serverOverrideIsSet(key) ? '已设置覆盖' : '继承全局')
}

function serverOpenlistOverrideStatus() {
  const addressOverride = serverOverrideIsSet('openlist_addr_override')
  const tokenOverride = serverOverrideIsSet('openlist_token_override')
  if (addressOverride && tokenOverride) return t('地址与 Token 均覆盖')
  if (addressOverride) return t('地址覆盖，Token 继承全局')
  if (tokenOverride) return t('地址继承全局，Token 覆盖')
  return t('继承全局')
}

function serverOpenlistOverridesSet() {
  return (
    serverOverrideIsSet('openlist_addr_override') || serverOverrideIsSet('openlist_token_override')
  )
}

function clearServerOpenlistOverrides() {
  const draft = serverDraft.value
  if (!draft) return
  draft.openlist_addr_override = null
  draft.openlist_token_override = null
  draft.openlist_token_override_configured = false
}

function clearServerOverride(key: ServerPolicyOverrideKey) {
  const draft = serverDraft.value
  if (!draft) return
  switch (key) {
    case 'openlist_addr_override':
      draft.openlist_addr_override = null
      break
    case 'openlist_token_override':
      draft.openlist_token_override = null
      draft.openlist_token_override_configured = false
      break
    case 'strm_url_mappings_override':
      draft.strm_url_mappings_override = null
      break
    case 'strm_url_mapping_enabled_override':
      draft.strm_url_mapping_enabled_override = null
      break
    case 'cache_enabled_override':
      draft.cache_enabled_override = null
      break
    case 'cache_ttl_seconds_override':
      draft.cache_ttl_seconds_override = null
      break
    case 'cache_max_capacity_override':
      draft.cache_max_capacity_override = null
      break
    case 'cache_domain_filter_mode_override':
      draft.cache_domain_filter_mode_override = null
      break
    case 'cache_domain_whitelist_override':
      draft.cache_domain_whitelist_override = null
      break
    case 'enable_internal_redirect_override':
      draft.enable_internal_redirect_override = null
      break
    case 'internal_redirect_timeout_seconds_override':
      draft.internal_redirect_timeout_seconds_override = null
      break
    case 'connectivity_check_enabled_override':
      draft.connectivity_check_enabled_override = null
      break
    case 'connectivity_check_interval_seconds_override':
      draft.connectivity_check_interval_seconds_override = null
      break
    case 'connectivity_check_timeout_seconds_override':
      draft.connectivity_check_timeout_seconds_override = null
      break
    case 'connectivity_auto_restart_seconds_override':
      draft.connectivity_auto_restart_seconds_override = null
      break
  }
}

function setServerOverrideString(key: StringServerOverrideKey, value: string) {
  const draft = serverDraft.value
  if (!draft) return
  switch (key) {
    case 'openlist_addr_override':
      draft.openlist_addr_override = value
      break
    case 'openlist_token_override':
      if (value.trim()) {
        draft.openlist_token_override = value
        draft.openlist_token_override_configured = true
      }
      break
    case 'strm_url_mappings_override':
      draft.strm_url_mappings_override = value
      break
    case 'cache_domain_whitelist_override':
      draft.cache_domain_whitelist_override = value
      break
  }
}

function setServerOverrideNumber(key: NumberServerOverrideKey, value: number | null) {
  const draft = serverDraft.value
  if (!draft) return
  switch (key) {
    case 'cache_ttl_seconds_override':
      draft.cache_ttl_seconds_override = value
      break
    case 'cache_max_capacity_override':
      draft.cache_max_capacity_override = value
      break
    case 'internal_redirect_timeout_seconds_override':
      draft.internal_redirect_timeout_seconds_override = value
      break
    case 'connectivity_check_interval_seconds_override':
      draft.connectivity_check_interval_seconds_override = value
      break
    case 'connectivity_check_timeout_seconds_override':
      draft.connectivity_check_timeout_seconds_override = value
      break
    case 'connectivity_auto_restart_seconds_override':
      draft.connectivity_auto_restart_seconds_override = value
      break
  }
}

function setServerOverrideBoolean(key: BooleanServerOverrideKey, value: boolean) {
  const draft = serverDraft.value
  if (!draft) return
  switch (key) {
    case 'strm_url_mapping_enabled_override':
      draft.strm_url_mapping_enabled_override = value
      break
    case 'cache_enabled_override':
      draft.cache_enabled_override = value
      break
    case 'enable_internal_redirect_override':
      draft.enable_internal_redirect_override = value
      break
    case 'connectivity_check_enabled_override':
      draft.connectivity_check_enabled_override = value
      break
  }
}

function setServerOverrideSelect(key: SelectServerOverrideKey, value: string) {
  if (serverDraft.value && ['off', 'whitelist', 'blacklist'].includes(value)) {
    serverDraft.value[key] = value
  }
}

function updateServerOverrideString(key: StringServerOverrideKey, event: Event) {
  const target = event.target
  if (!(target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement)) return
  setServerOverrideString(key, target.value)
}

function updateServerOverrideNumber(key: NumberServerOverrideKey, event: Event) {
  const target = event.target
  if (!(target instanceof HTMLInputElement)) return
  setServerOverrideNumber(key, target.value === '' ? null : Number(target.value))
}

function serverOverrideText(key: StringServerOverrideKey, globalValue: string | null | undefined) {
  const draft = serverDraft.value
  if (!draft) return ''
  const value = draft[key]
  if (key === 'openlist_token_override') return value == null ? '' : String(value)
  return value == null ? (globalValue ?? '') : String(value)
}

function serverOverrideNumber(key: NumberServerOverrideKey, globalValue: number) {
  const value = serverDraft.value?.[key]
  return value == null ? globalValue : Number(value)
}

function serverOverrideBoolean(key: BooleanServerOverrideKey, globalValue: boolean) {
  const value = serverDraft.value?.[key]
  return value == null ? globalValue : Boolean(value)
}

function serverOverrideSelect(key: SelectServerOverrideKey, globalValue: string) {
  const value = serverDraft.value?.[key]
  return value == null ? globalValue : String(value)
}

async function applyServerEditor() {
  if (saving.value) return
  const draft = serverDraft.value
  if (!draft) return
  serverEditorError.value = validateServerDraft(draft)
  if (serverEditorError.value) return
  const target = settings.servers.find((server) => server.id === draft.id)
  if (target) Object.assign(target, draft)
  error.value = ''
  serverEditorError.value = ''
  await saveSettings()
  if (error.value) {
    serverEditorError.value = error.value
    error.value = ''
    return
  }
  serverEditorIsNew.value = false
  serverEditorOpen.value = false
}

async function runValidation() {
  if (saving.value || validationRunning.value) return
  validationDialogOpen.value = true
  validationRunning.value = true
  try {
    await validateSettings()
  } finally {
    validationRunning.value = false
  }
}

async function removeServerAndSave(serverId: string) {
  if (saving.value || removingServerId.value) return
  const server = settings.servers.find((item) => item.id === serverId)
  if (!server) return
  removingServerId.value = serverId
  try {
    const confirmed = await confirmAction({
      title: t('删除服务器'),
      description: `${server.name || t('服务器')}：${t('确定删除这个服务器配置吗？对应反代端口保存后会停止监听。')}`,
      confirmText: t('确认删除'),
      cancelText: t('取消'),
      tone: 'danger',
    })
    if (!confirmed || saving.value) return
    const previousCount = settings.servers.length
    removeServer(serverId)
    if (settings.servers.length === previousCount) return
    await saveSettings()
  } finally {
    removingServerId.value = ''
  }
}
</script>

<template>
  <div class="panel">
    <div class="panel-head">
      <div>
        <div class="panel-title-line">
          <Server :size="18" />
          <h2>{{ t('服务器配置') }}</h2>
        </div>
        <p class="muted">
          {{ t('每个 Emby 服务器使用独立反代端口，修改后自动保存并同步监听。') }}
        </p>
      </div>
      <div class="panel-actions">
        <button
          class="secondary"
          type="button"
          :disabled="saving"
          @click="addServerAndEdit"
        >
          <Plus :size="15" />{{ t('添加服务器') }}
        </button>
        <button
          class="secondary"
          type="button"
          :disabled="saving"
          aria-haspopup="dialog"
          @click="runValidation"
        >
          <ShieldCheck :size="15" />{{ t('测试配置') }}
        </button>
      </div>
    </div>
    <div class="server-list">
      <article
        v-for="(server, index) in settings.servers"
        :key="server.id"
        class="server-card"
      >
        <div class="server-card-head">
          <div class="server-card-identity">
            <strong>{{ server.name || `${t('服务器')} ${index + 1}` }}</strong>
            <span class="server-card-endpoint">
              {{ server.emby_host || t('未配置 Emby 地址') }}
            </span>
          </div>
          <div class="server-actions">
            <button
              type="button"
              class="secondary"
              :disabled="saving"
              aria-haspopup="dialog"
              @click="openServerEditor(server)"
            >
              <Settings2 :size="15" />{{ t('编辑配置') }}
            </button>
            <button
              type="button"
              :class="['server-toggle-button', { disabled: !server.enabled }]"
              :disabled="saving || restartingServerId === server.id"
              :aria-pressed="server.enabled"
              @click="toggleProxyServer(server)"
            >
              <Zap :size="15" />{{
                server.enabled ? t('关闭服务器') : t('开启服务器')
              }}
            </button>
            <button
              type="button"
              class="secondary restart-button"
              :disabled="
                saving || restartingServerId === server.id || !server.enabled
              "
              @click="restartProxyServer(server)"
            >
              <RotateCw :size="15" />{{
                restartingServerId === server.id ? t('重启中') : t('重启服务器')
              }}
            </button>
            <button
              class="danger-button"
              type="button"
              :disabled="saving || removingServerId !== ''"
              @click="removeServerAndSave(server.id)"
            >
              <Trash2 :size="15" />{{ t('删除') }}
            </button>
          </div>
        </div>
        <div class="server-status-strip">
          <span
            :class="[
              'client-badge',
              proxyStatusById[server.id]?.listening ? 'allowed' : 'blocked',
            ]"
          >
            {{ proxyStatusLabel(proxyStatusById[server.id]) }}
          </span>
          <span
            >{{ t('端口') }} :{{
              proxyStatusById[server.id]?.port || server.port
            }}</span
          >
          <span
            >{{ t('启动') }}
            {{
              formatTimestampMs(proxyStatusById[server.id]?.started_at_ms)
            }}</span
          >
          <span
            >{{ t('最近请求') }}
            {{
              formatTimestampMs(proxyStatusById[server.id]?.last_request_ms)
            }}</span
          >
          <span
            v-if="proxyStatusById[server.id]?.last_error"
            class="server-status-error"
          >
            {{ proxyStatusById[server.id]?.last_error }}
          </span>
        </div>
      </article>
    </div>

    <section class="settings-card-section" aria-labelledby="routing-settings-title">
      <div class="config-section-head">
        <div class="config-section-title">
          <Route :size="16" aria-hidden="true" />
          <h3 id="routing-settings-title">{{ t('直链与重定向') }}</h3>
        </div>
      </div>
      <div class="settings-card-grid">
        <article
          v-for="card in routingSettingCards"
          :key="card.key"
          class="settings-card"
          :class="{ 'is-disabled': isSettingCardDisabled(card) || saving }"
        >
          <component
            :is="card.editor === 'boolean' ? 'div' : 'button'"
            class="settings-card-main"
            :type="card.editor === 'boolean' ? undefined : 'button'"
            :disabled="card.editor === 'boolean' ? undefined : isSettingCardDisabled(card) || saving"
            :aria-haspopup="card.editor === 'boolean' ? undefined : 'dialog'"
            @click="handleSettingCardClick(card)"
          >
            <span class="settings-card-icon" aria-hidden="true">
              <component :is="card.icon" :size="18" />
            </span>
            <span class="settings-card-copy">
              <span class="settings-card-label">{{ t(card.label) }}</span>
              <strong class="settings-card-value">{{ settingSummary(card) }}</strong>
            </span>
            <ChevronRight
              v-if="card.editor !== 'boolean'"
              class="settings-card-chevron"
              :size="17"
              aria-hidden="true"
            />
          </component>
          <UiSwitch
            v-if="card.toggleKey"
            :model-value="settingBooleanValue(card)"
            :label="`${t(card.label)}：${t(settingBooleanValue(card) ? '已开启' : '已关闭')}`"
            :disabled="saving"
            @update:model-value="toggleBooleanSetting(card)"
          />
        </article>
      </div>
    </section>

    <section class="settings-card-section" aria-labelledby="cache-settings-title">
      <div class="config-section-head">
        <div class="config-section-title">
          <Database :size="16" aria-hidden="true" />
          <h3 id="cache-settings-title">{{ t('缓存与巡检') }}</h3>
        </div>
      </div>
      <div class="settings-card-grid">
        <article
          v-for="card in cacheSettingCards"
          :key="card.key"
          class="settings-card"
          :class="{ 'is-disabled': saving }"
        >
          <component
            :is="card.editor === 'boolean' ? 'div' : 'button'"
            class="settings-card-main"
            :type="card.editor === 'boolean' ? undefined : 'button'"
            :disabled="card.editor === 'boolean' ? undefined : saving"
            :aria-haspopup="card.editor === 'boolean' ? undefined : 'dialog'"
            @click="handleSettingCardClick(card)"
          >
            <span class="settings-card-icon" aria-hidden="true">
              <component :is="card.icon" :size="18" />
            </span>
            <span class="settings-card-copy">
              <span class="settings-card-label">{{ t(card.label) }}</span>
              <strong class="settings-card-value">{{ settingSummary(card) }}</strong>
            </span>
            <ChevronRight
              v-if="card.editor !== 'boolean'"
              class="settings-card-chevron"
              :size="17"
              aria-hidden="true"
            />
          </component>
          <UiSwitch
            v-if="card.toggleKey"
            :model-value="settingBooleanValue(card)"
            :label="`${t(card.label)}：${t(settingBooleanValue(card) ? '已开启' : '已关闭')}`"
            :disabled="saving"
            @update:model-value="toggleBooleanSetting(card)"
          />
        </article>
      </div>
    </section>

    <SettingsDialogShell
      v-if="activeSettingCard"
      v-model:open="settingEditorOpen"
      :title="t(activeSettingCard.label)"
      :description="t('点击应用后会自动保存配置。')"
      :close-label="t('关闭')"
    >
      <template #icon>
            <component :is="activeSettingCard.icon" :size="19" />
      </template>

        <form class="settings-dialog-form" @submit.prevent="applySettingEditor">
          <label v-if="activeSettingCard.editor === 'select'">
            <span>{{ t(activeSettingCard.label) }}</span>
            <select v-model="editorTextDraft">
              <option
                v-for="option in activeSettingCard.options"
                :key="option.value"
                :value="option.value"
              >
                {{ t(option.label) }}
              </option>
            </select>
          </label>

          <label v-else-if="activeSettingCard.editor === 'number'">
            <span>{{ t(activeSettingCard.label) }}</span>
            <input
              v-model.number="editorNumberDraft"
              type="number"
              required
              step="1"
              :min="activeSettingCard.min"
              :max="activeSettingCard.max"
            />
          </label>

          <label v-else-if="activeSettingCard.editor === 'textarea'">
            <span>{{ t(activeSettingCard.label) }}</span>
            <textarea
              v-model="editorTextDraft"
              class="setting-dialog-textarea"
              rows="6"
              spellcheck="false"
              :placeholder="settingPlaceholder(activeSettingCard)"
            />
          </label>

          <label v-else>
            <span>{{ t(activeSettingCard.label) }}</span>
            <input
              v-model="editorTextDraft"
              :type="activeSettingCard.editor"
              autocomplete="off"
              :placeholder="settingPlaceholder(activeSettingCard)"
            />
          </label>

          <small v-if="activeSettingCard.help" class="field-help">
            {{ t(activeSettingCard.help) }}
          </small>

          <div class="settings-dialog-actions">
            <DialogClose as-child>
              <button class="secondary" type="button">{{ t('取消') }}</button>
            </DialogClose>
            <button class="primary" type="submit" :disabled="saving">
              <Check :size="15" aria-hidden="true" />{{ t('保存') }}
            </button>
          </div>
          </form>
    </SettingsDialogShell>

    <SettingsDialogShell
      v-model:open="validationDialogOpen"
      :title="t('配置测试结果')"
      :description="t('测试结果按配置检查顺序显示。')"
      :close-label="t('关闭')"
      content-class="validation-dialog-content"
      show-description
    >
      <template #icon>
        <ShieldCheck :size="19" />
      </template>

      <div
        class="validation-dialog-body"
        aria-live="polite"
        :aria-busy="validationRunning"
      >
        <div v-if="validationRunning" class="validation-dialog-progress">
          <LoaderCircle
            class="secret-toggle-spinner"
            :size="18"
            aria-hidden="true"
          />
          <span>{{ t('测试中') }}</span>
        </div>
        <p v-else-if="error" class="notice error" role="alert">{{ error }}</p>
        <div
          v-else-if="validationResults.length"
          class="validation-list"
          role="list"
        >
          <div
            v-for="result in validationResults"
            :key="`${result.scope}-${result.message}-${result.detail}`"
            :class="['validation-row', validationClass(result)]"
            role="listitem"
          >
            <strong>{{ localizeValidationText(result.scope) }}</strong>
            <span>{{ localizeValidationText(result.message) }}</span>
            <small>{{
              result.detail ? localizeValidationText(result.detail) : '--'
            }}</small>
          </div>
        </div>
        <div v-else class="empty-state compact">
          {{ t('还没有运行配置测试。') }}
        </div>
      </div>

      <template #footer>
        <DialogClose as-child>
          <button class="secondary" type="button">{{ t('关闭') }}</button>
        </DialogClose>
        <button
          class="primary"
          type="button"
          :disabled="saving || validationRunning"
          @click="runValidation"
        >
          <LoaderCircle
            v-if="validationRunning"
            class="secret-toggle-spinner"
            :size="15"
            aria-hidden="true"
          />
          <RefreshCw v-else :size="15" aria-hidden="true" />
          {{ validationRunning ? t('测试中') : t('重新测试') }}
        </button>
      </template>
    </SettingsDialogShell>

    <SettingsDialogShell
      v-if="serverDraft"
      :open="serverEditorOpen"
      :title="t(serverEditorIsNew ? '添加服务器' : '编辑服务器配置')"
      :description="t('点击应用后会自动保存配置。')"
      :close-label="t('关闭')"
      content-class="server-dialog-content"
      @update:open="handleServerEditorOpenChange"
    >
      <template #icon>
              <Server :size="19" />
      </template>

          <form
            class="settings-dialog-form server-dialog-form"
            @submit.prevent="applyServerEditor"
          >
            <p
              v-if="serverEditorError"
              class="notice error server-editor-error"
              role="alert"
            >
              {{ serverEditorError }}
            </p>
            <div class="grid server-grid">
              <label>
                <span>{{ t('名称') }}</span>
                <input
                  v-model="serverDraft.name"
                  :placeholder="t('例如：主服务器')"
                />
              </label>
              <label>
                <span>{{ t('Emby 地址') }}</span>
                <input
                  v-model="serverDraft.emby_host"
                  placeholder="http://emby.local:8096"
                />
              </label>
              <label>
                <span>Emby API Key</span>
                <div class="secret-input">
                  <input
                    :value="apiKeyInputValue(serverDraft)"
                    :type="isApiKeyVisible(serverDraft.id) ? 'text' : 'password'"
                    :placeholder="apiKeyPlaceholder(serverDraft)"
                    autocomplete="off"
                    @input="updateApiKeyInput(serverDraft, $event)"
                  />
                  <button
                    type="button"
                    class="secret-toggle"
                    :disabled="isApiKeyRevealLoading(serverDraft.id)"
                    :aria-pressed="isApiKeyVisible(serverDraft.id)"
                    :aria-label="
                      isApiKeyVisible(serverDraft.id)
                        ? t('隐藏 Emby API Key')
                        : t('显示 Emby API Key')
                    "
                    :title="
                      isApiKeyVisible(serverDraft.id)
                        ? t('隐藏 Emby API Key')
                        : t('显示 Emby API Key')
                    "
                    @click="toggleApiKeyVisible(serverDraft)"
                  >
                    <LoaderCircle
                      v-if="isApiKeyRevealLoading(serverDraft.id)"
                      class="secret-toggle-spinner"
                      :size="16"
                      aria-hidden="true"
                    />
                    <EyeOff
                      v-else-if="!isApiKeyVisible(serverDraft.id)"
                      :size="16"
                      aria-hidden="true"
                    />
                    <Eye v-else :size="16" aria-hidden="true" />
                  </button>
                </div>
              </label>
              <label>
                <span>{{ t('反代端口') }}</span>
                <input
                  v-model.number="serverDraft.port"
                  type="number"
                  min="1"
                  max="65535"
                  required
                />
              </label>
            </div>

            <CheckboxField
              v-model="serverDraft.block_web_ui"
              class="server-option"
              :label="t('屏蔽 Emby Web UI')"
            />
            <div v-if="serverDraft.block_web_ui" class="grid real-ip-grid web-ui-block-settings">
              <label>
                <span>{{ t('拦截状态码') }}</span>
                <select v-model.number="serverDraft.block_web_ui_status_code">
                  <option v-for="status in [400, 401, 403, 404, 429, 500, 503]" :key="status" :value="status">
                    {{ status }}
                  </option>
                </select>
              </label>
              <label>
                <span>{{ t('拦截提示词') }}</span>
                <textarea
                  v-model="serverDraft.block_web_ui_message"
                  maxlength="512"
                  :placeholder="t('留空时使用默认提示词')"
                />
                <small class="field-help">{{ t('仅修改反代拦截响应，不影响 Emby API 和播放。') }}</small>
              </label>
            </div>

            <div class="grid real-ip-grid">
              <label>
                <span>{{ t('真实 IP 获取方式') }}</span>
                <select
                  v-model="serverDraft.real_ip_mode"
                  @change="updateRealIpMode(serverDraft)"
                >
                  <option
                    v-for="option in realIpModeOptions"
                    :key="option.value"
                    :value="option.value"
                  >
                    {{ t(option.label) }}
                  </option>
                </select>
                <small
                  v-if="serverDraft.real_ip_mode === 'header_list'"
                  class="field-help"
                >
                  {{
                    t(
                      '从下列常用 CDN 携带真实 IP 的 HTTP Header 中获取，按顺序取第一个能获取到的值。',
                    )
                  }}
                </small>
              </label>
              <label v-if="needsRealIpHeader(serverDraft)">
                <span>{{
                  serverDraft.real_ip_mode === 'header'
                    ? 'HTTP Header'
                    : 'CDN Headers'
                }}</span>
                <textarea
                  v-model="serverDraft.real_ip_header"
                  :placeholder="
                    serverDraft.real_ip_mode === 'header'
                      ? t('例如：x-real-ip')
                      : defaultCdnHeaders
                  "
                />
              </label>
              <label>
                <span>{{ t('可信代理 IP/CIDR') }}</span>
                <textarea
                  v-model="serverDraft.trusted_proxy_cidrs"
                  :placeholder="t('例如：10.0.0.0/8，每行一个')"
                />
              </label>
            </div>
            <p class="muted real-ip-help">
              {{
                t(
                  '默认使用系统识别。经过 CDN 或多层反代后 IP 不准时再配置，修改后会同步重启对应反代服务。',
                )
              }}
            </p>

            <section class="server-policy-overrides" aria-labelledby="server-policy-overrides-title">
          <div class="config-section-head">
            <div class="config-section-title">
              <Settings2 :size="16" aria-hidden="true" />
              <h3 id="server-policy-overrides-title">
                {{ t('服务器策略覆盖') }}
              </h3>
            </div>
          </div>
          <p class="muted real-ip-help">
            {{ t('未设置=继承全局；设置覆盖后仅影响当前服务器，可随时清除回继承。') }}
          </p>

          <div class="grid server-policy-grid">
            <div class="server-policy-field">
              <div class="server-policy-field-head">
                <span>{{ t('OpenList 地址') }}</span>
                <span class="muted">{{ serverOpenlistOverrideStatus() }}</span>
                <button
                  v-if="serverOpenlistOverridesSet()"
                  class="secondary"
                  type="button"
                  @click="clearServerOpenlistOverrides"
                >
                  {{ t('恢复全局') }}
                </button>
              </div>
              <input
                :value="serverOverrideText('openlist_addr_override', null)"
                type="url"
                :placeholder="settings.openlist_addr || t('全局未配置')"
                @input="updateServerOverrideString('openlist_addr_override', $event)"
              />
            </div>

            <div class="server-policy-field">
              <div class="server-policy-field-head">
                <span>{{ t('OpenList Token') }}</span>
              </div>
              <input
                :value="serverOverrideText('openlist_token_override', null)"
                type="password"
                autocomplete="new-password"
                :placeholder="
                  serverDraft.openlist_token_override_configured
                    ? t('已配置，输入新 Token 可替换')
                    : t('继承全局 Token')
                "
                @input="updateServerOverrideString('openlist_token_override', $event)"
              />
              <small class="field-help">
                {{
                  t(
                    '地址与 Token 按最终有效配置成对使用；允许只覆盖其中一项并继承另一项。恢复全局会同时清除两项覆盖。',
                  )
                }}
              </small>
              <small class="field-help">{{
                t('Token 只显示为密码框；留空且不清除时保持已有安全值。')
              }}</small>
            </div>

            <div class="server-policy-field">
              <div class="server-policy-field-head">
                <span>{{ t('STRM URL 映射') }}</span>
                <span class="muted">{{ serverOverrideStatus('strm_url_mappings_override') }}</span>
                <button
                  v-if="serverOverrideIsSet('strm_url_mappings_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('strm_url_mappings_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <textarea
                :value="serverOverrideText('strm_url_mappings_override', null)"
                :placeholder="settings.strm_url_mappings || strmMappingPlaceholder"
                @input="updateServerOverrideString('strm_url_mappings_override', $event)"
              />
            </div>

            <div class="server-policy-field server-policy-toggle-field">
              <div class="server-policy-field-head">
                <span>{{ t('启用 STRM URL 映射') }}</span>
                <span class="muted">{{
                  serverOverrideStatus('strm_url_mapping_enabled_override')
                }}</span>
                <button
                  v-if="serverOverrideIsSet('strm_url_mapping_enabled_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('strm_url_mapping_enabled_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <UiSwitch
                :model-value="
                  serverOverrideBoolean(
                    'strm_url_mapping_enabled_override',
                    settings.strm_url_mapping_enabled,
                  )
                "
                :label="t('启用 STRM URL 映射')"
                @update:model-value="
                  setServerOverrideBoolean('strm_url_mapping_enabled_override', $event)
                "
              />
            </div>

            <div class="server-policy-field server-policy-toggle-field">
              <div class="server-policy-field-head">
                <span>{{ t('启用缓存') }}</span>
                <span class="muted">{{ serverOverrideStatus('cache_enabled_override') }}</span>
                <button
                  v-if="serverOverrideIsSet('cache_enabled_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('cache_enabled_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <UiSwitch
                :model-value="
                  serverOverrideBoolean('cache_enabled_override', settings.cache_enabled)
                "
                :label="t('启用缓存')"
                @update:model-value="setServerOverrideBoolean('cache_enabled_override', $event)"
              />
            </div>

            <div class="server-policy-field">
              <div class="server-policy-field-head">
                <span>{{ t('缓存秒数') }}</span>
                <span class="muted">{{ serverOverrideStatus('cache_ttl_seconds_override') }}</span>
                <button
                  v-if="serverOverrideIsSet('cache_ttl_seconds_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('cache_ttl_seconds_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <input
                :value="
                  serverOverrideNumber('cache_ttl_seconds_override', settings.cache_ttl_seconds)
                "
                type="number"
                min="0"
                @input="updateServerOverrideNumber('cache_ttl_seconds_override', $event)"
              />
            </div>

            <div class="server-policy-field">
              <div class="server-policy-field-head">
                <span>{{ t('缓存最大条数') }}</span>
                <span class="muted">{{ serverOverrideStatus('cache_max_capacity_override') }}</span>
                <button
                  v-if="serverOverrideIsSet('cache_max_capacity_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('cache_max_capacity_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <input
                :value="
                  serverOverrideNumber('cache_max_capacity_override', settings.cache_max_capacity)
                "
                type="number"
                min="1"
                @input="updateServerOverrideNumber('cache_max_capacity_override', $event)"
              />
            </div>

            <div class="server-policy-field">
              <div class="server-policy-field-head">
                <span>{{ t('缓存过滤模式') }}</span>
                <span class="muted">{{
                  serverOverrideStatus('cache_domain_filter_mode_override')
                }}</span>
                <button
                  v-if="serverOverrideIsSet('cache_domain_filter_mode_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('cache_domain_filter_mode_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <select
                :value="
                  serverOverrideSelect(
                    'cache_domain_filter_mode_override',
                    settings.cache_domain_filter_mode,
                  )
                "
                @change="
                  setServerOverrideSelect(
                    'cache_domain_filter_mode_override',
                    ($event.target as HTMLSelectElement).value,
                  )
                "
              >
                <option value="off">{{ t('不过滤') }}</option>
                <option value="whitelist">{{ t('白名单：命中才缓存') }}</option>
                <option value="blacklist">{{ t('黑名单：命中不缓存') }}</option>
              </select>
            </div>

            <div class="server-policy-field">
              <div class="server-policy-field-head">
                <span>{{ t('缓存过滤域名') }}</span>
                <span class="muted">{{
                  serverOverrideStatus('cache_domain_whitelist_override')
                }}</span>
                <button
                  v-if="serverOverrideIsSet('cache_domain_whitelist_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('cache_domain_whitelist_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <textarea
                :value="serverOverrideText('cache_domain_whitelist_override', null)"
                :placeholder="settings.cache_domain_whitelist || t('全局未配置')"
                @input="updateServerOverrideString('cache_domain_whitelist_override', $event)"
              />
            </div>

            <div class="server-policy-field server-policy-toggle-field">
              <div class="server-policy-field-head">
                <span>{{ t('开启内部重定向 HEAD 解析') }}</span>
                <span class="muted">{{
                  serverOverrideStatus('enable_internal_redirect_override')
                }}</span>
                <button
                  v-if="serverOverrideIsSet('enable_internal_redirect_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('enable_internal_redirect_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <UiSwitch
                :model-value="
                  serverOverrideBoolean(
                    'enable_internal_redirect_override',
                    settings.enable_internal_redirect,
                  )
                "
                :label="t('开启内部重定向 HEAD 解析')"
                @update:model-value="
                  setServerOverrideBoolean('enable_internal_redirect_override', $event)
                "
              />
            </div>

            <div class="server-policy-field">
              <div class="server-policy-field-head">
                <span>{{ t('HEAD 超时秒数') }}</span>
                <span class="muted">{{
                  serverOverrideStatus('internal_redirect_timeout_seconds_override')
                }}</span>
                <button
                  v-if="serverOverrideIsSet('internal_redirect_timeout_seconds_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('internal_redirect_timeout_seconds_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <input
                :value="
                  serverOverrideNumber(
                    'internal_redirect_timeout_seconds_override',
                    settings.internal_redirect_timeout_seconds,
                  )
                "
                type="number"
                min="1"
                @input="
                  updateServerOverrideNumber('internal_redirect_timeout_seconds_override', $event)
                "
              />
            </div>

            <div class="server-policy-field server-policy-toggle-field">
              <div class="server-policy-field-head">
                <span>{{ t('启用服务器连通性巡检') }}</span>
                <span class="muted">{{
                  serverOverrideStatus('connectivity_check_enabled_override')
                }}</span>
                <button
                  v-if="serverOverrideIsSet('connectivity_check_enabled_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('connectivity_check_enabled_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <UiSwitch
                :model-value="
                  serverOverrideBoolean(
                    'connectivity_check_enabled_override',
                    settings.connectivity_check_enabled,
                  )
                "
                :label="t('启用服务器连通性巡检')"
                @update:model-value="
                  setServerOverrideBoolean('connectivity_check_enabled_override', $event)
                "
              />
            </div>

            <div class="server-policy-field">
              <div class="server-policy-field-head">
                <span>{{ t('巡检间隔秒数') }}</span>
                <span class="muted">{{
                  serverOverrideStatus('connectivity_check_interval_seconds_override')
                }}</span>
                <button
                  v-if="serverOverrideIsSet('connectivity_check_interval_seconds_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('connectivity_check_interval_seconds_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <input
                :value="
                  serverOverrideNumber(
                    'connectivity_check_interval_seconds_override',
                    settings.connectivity_check_interval_seconds,
                  )
                "
                type="number"
                min="10"
                max="3600"
                @input="
                  updateServerOverrideNumber('connectivity_check_interval_seconds_override', $event)
                "
              />
            </div>

            <div class="server-policy-field">
              <div class="server-policy-field-head">
                <span>{{ t('单项超时秒数') }}</span>
                <span class="muted">{{
                  serverOverrideStatus('connectivity_check_timeout_seconds_override')
                }}</span>
                <button
                  v-if="serverOverrideIsSet('connectivity_check_timeout_seconds_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('connectivity_check_timeout_seconds_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <input
                :value="
                  serverOverrideNumber(
                    'connectivity_check_timeout_seconds_override',
                    settings.connectivity_check_timeout_seconds,
                  )
                "
                type="number"
                min="1"
                max="60"
                @input="
                  updateServerOverrideNumber('connectivity_check_timeout_seconds_override', $event)
                "
              />
            </div>

            <div class="server-policy-field">
              <div class="server-policy-field-head">
                <span>{{ t('反代无响应自动重启秒数') }}</span>
                <span class="muted">{{
                  serverOverrideStatus('connectivity_auto_restart_seconds_override')
                }}</span>
                <button
                  v-if="serverOverrideIsSet('connectivity_auto_restart_seconds_override')"
                  class="secondary"
                  type="button"
                  @click="clearServerOverride('connectivity_auto_restart_seconds_override')"
                >
                  {{ t('清除覆盖') }}
                </button>
              </div>
              <input
                :value="
                  serverOverrideNumber(
                    'connectivity_auto_restart_seconds_override',
                    settings.connectivity_auto_restart_seconds,
                  )
                "
                type="number"
                min="0"
                max="86400"
                @input="
                  updateServerOverrideNumber('connectivity_auto_restart_seconds_override', $event)
                "
              />
            </div>
          </div>
        </section>

        <div class="settings-dialog-actions">
          <DialogClose as-child>
            <button class="secondary" type="button">{{ t('取消') }}</button>
          </DialogClose>
          <button class="primary" type="submit" :disabled="saving">
            <Check :size="15" aria-hidden="true" />{{ t('保存') }}
          </button>
        </div>
      </form>
    </SettingsDialogShell>
  </div>
</template>
