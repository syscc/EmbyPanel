<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { ChevronLeft, ChevronRight, Clock3, Film, GitBranch, Monitor, RefreshCw, RotateCcw, Search, Server, Timer, Trash2, TriangleAlert } from '@lucide/vue'
import { useDiagnosticsController, diagnosisShareRecord, type DiagnosisRecord, type DiagnosisStep } from '@/composables/useDiagnosticsController'
import { usePanelContext } from '@/composables/panel-context'
import RefreshButton from '@/components/ui/RefreshButton.vue'
import SettingsDialogShell from '@/components/ui/SettingsDialogShell.vue'
import { formatTimestampMs } from '@/lib/panel-formatters'

const { t, locale, showNotice, clearNotice, api, confirmAction, copyText, openDiagnosticLogs } = usePanelContext()
const controller = useDiagnosticsController({ api, translate: t, showNotice, clearNotice, confirmAction })
const {
  diagnostics, loading, cleaning, canGlobalCleanup, error, query, hasNextPage, total, storage, loadedPage,
  loadDiagnostics, applyFilters, resetFilters, nextPage, prevPage, cleanDiagnostics, dispose,
} = controller

const selectedRecord = ref<DiagnosisRecord | null>(null)
const detailOpen = ref(false)
const pageSummary = computed(() => ({
  hits: diagnostics.value.filter((record) => record.cache_hit).length,
  issues: diagnostics.value.filter(hasIssue).length,
}))

function diagnosisTime(value: number | null | undefined) {
  return formatTimestampMs(value, locale.value, {
    year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit', second: '2-digit',
    fractionalSecondDigits: 3, hour12: false, timeZoneName: 'shortOffset',
  })
}

function hasIssue(record: DiagnosisRecord) {
  return Boolean(record.failure_node || record.sanitized_errors.length || ['failed', 'cancelled', 'blocked'].includes(record.result?.outcome ?? '') || record.steps.some((step) => step.state === 'failed' || step.state === 'cancelled'))
}

function isCancelled(record: DiagnosisRecord) {
  return record.result?.outcome === 'cancelled' || record.steps.some((step) => step.name === record.failure_node
    && (step.details.startsWith('cancelled:') || step.details === 'cancelled before step started'))
}

function issueLabel(record: DiagnosisRecord) {
  if (!record.failure_node) return isCancelled(record) ? t('已取消') : record.result?.outcome === 'blocked' ? t('已拦截') : t('异常')
  return `${t(isCancelled(record) ? '已取消' : '失败节点')}: ${t(record.failure_node)}`
}

function stepState(record: DiagnosisRecord, step: DiagnosisStep) {
  if (step.state) return step.state
  if (record.failure_node === step.name) return isCancelled(record) ? 'cancelled' : 'failed'
  return step.details === 'bypassed' ? 'skipped' : 'recorded'
}

function stepStateLabel(record: DiagnosisRecord, step: DiagnosisStep) {
  const state = stepState(record, step)
  if (state === 'cancelled') return t('已取消')
  return state === 'failed' ? t('异常') : state === 'skipped' ? t('已跳过') : t('已记录')
}

const outcomeOptions = [
  { value: 'redirect', label: '已返回跳转' }, { value: 'proxy', label: '代理响应' },
  { value: 'failed', label: '失败' }, { value: 'cancelled', label: '已取消' }, { value: 'blocked', label: '已拦截' },
] as const

function outcomeLabel(record: DiagnosisRecord) {
  return t(outcomeOptions.find((option) => option.value === record.result?.outcome)?.label ?? '未采集')
}

function cacheLabel(record: DiagnosisRecord) {
  const status = record.cache?.status
  if (status === 'hit' || (!status && record.cache_hit)) return t('缓存命中')
  return t(status === 'miss' ? '未命中' : status === 'disabled' ? '缓存已禁用' : status === 'not_checked' ? '未检查' : '未采集')
}

function milliseconds(value: number | null | undefined) {
  return value == null ? t('未采集') : `${value} ms`
}

function connectionObservationLabel(value: DiagnosisStep['connection_observation']) {
  if (value === 'not_observed') return t('未观测到连接尝试')
  if (value === 'observed') return t('已观测到连接尝试')
  if (value === 'partial') return t('连接尝试观测不完整')
  return t('未知（旧数据或未提供）')
}

// Authorized details use backend-sanitized fields; sharing uses a separate strict allowlist.
function detailText(value: unknown) {
  return value == null || value === '' ? '—' : String(value)
}

const diagnosticMessages: Record<string, string> = {
  'cache hit; redirect response only': '缓存命中，已返回跳转',
  'direct media URL; redirect response only': '媒体为直链，已返回跳转',
  'OpenList raw URL; redirect response only': 'OpenList 解析为直链，已返回跳转',
  'client policy blocked request': '客户端策略已拦截请求',
  'concurrent playback limit': '命中同时播放限制',
  'playback rate limit': '命中播放频率限制',
  'cache policy disabled': '缓存策略未启用',
  'cached target accepted': '已使用可用缓存目标',
  'cached target rejected by domain policy': '缓存目标未通过域名策略',
  'no cached entry observed': '未找到可用缓存条目',
  'cache miss; entry inserted': '缓存未命中，已写入新条目',
  'cache insertion disabled by effective bucket policy': '当前缓存策略不允许写入',
  'target not accepted by domain whitelist': '目标未通过域名白名单',
  'target rejected by domain blacklist': '目标命中域名黑名单',
  'target URL is not cacheable': '目标地址不可缓存',
  'HEAD request does not enforce playback limits': 'HEAD 请求跳过播放限制检查',
  'URL mapping disabled': '路径映射未启用',
  'direct URL selected; OpenList API not configured': '已选择直链，未使用 OpenList API',
  'direct URL selected; no OpenList raw URL needed': '已选择直链，无需解析 raw_url',
  'cache hit; media path resolution not needed': '缓存命中，无需重新解析媒体路径',
  'cache hit; cached final URL reused': '缓存命中，复用跳转目标',
  'internal redirect disabled': '内部跳转解析未启用',
  'path requires Emby proxy, not direct URL resolution': '该路径需要回源 Emby',
  'path requires Emby proxy; no direct URL cached': '该路径需要回源，未缓存直链',
  'path is neither a direct URL nor OpenList /d path': '路径不是直链或 OpenList 下载路径',
  'media source path not found; using Emby proxy': '未找到媒体源路径，回源 Emby',
  'media source path not found; no cache insertion': '未找到媒体源路径，未写入缓存',
  'source not found; proxy to Emby': '媒体源未找到，回源 Emby',
  'requested media source path not found; proxy response headers only': '媒体源路径未找到，记录回源响应头结果',
  'path is neither a direct URL nor OpenList /d path; proxy response headers only': '路径需要回源，记录回源响应头结果',
  'using original URL after internal redirect failure': '内部跳转解析失败，继续使用原始地址',
  timeout: '请求超时', connect: '连接失败', decode: '响应解析失败', upstream_http: '上游 HTTP 错误',
  upstream_api: '上游 API 错误', upstream: '上游请求错误', invalid_url: '地址无效',
  missing_raw_url: '缺少 raw_url', not_found: '未找到媒体项目',
  invalid_redirect_location: '无法返回跳转地址',
}

function diagnosticMessage(value: unknown) {
  return typeof value === 'string' && Object.hasOwn(diagnosticMessages, value) ? t(diagnosticMessages[value]) : detailText(value)
}

const detailSections = computed(() => {
  const r = selectedRecord.value
  if (!r) return []
  const c = r.context, result = r.result, cache = r.cache, paths = r.paths, timing = r.timing
  return [
    { title: '基本请求与用户', rows: [
      ['服务器', c?.server_name], ['媒体', c?.media_name], ['剧集', c?.series_name],
      ['季号', c?.season_number], ['集号', c?.episode_number], ['媒体源 ID', c?.media_source_id],
      ['用户', c?.user_name], ['客户端', c?.client_name],
      ['设备', c?.device_name], ['设备 ID', c?.device_id],
      ['来源 IP', c?.source_ip], ['请求方法', c?.method],
      ['请求路径', c?.request_path], ['Range', c?.range],
    ] },
    { title: '处理结果与选择原因', rows: [
      ['处理结果', outcomeLabel(r)], ['响应状态码', result?.status_code], ['目标主机', result?.target_host],
      ['选择原因', result?.reason], ['错误类别', result?.error_kind], ['上游状态码', result?.upstream_status],
    ] },
    { title: '缓存', rows: [
      ['缓存状态', cacheLabel(r)], ['已写入缓存', cache?.stored == null ? null : t(cache.stored ? '是' : '否')],
      ['缓存原因', cache?.reason], ['有效期（秒）', cache?.ttl_seconds],
      ['剩余有效期', cache?.remaining_ttl_ms], ['目标主机', cache?.target_host],
    ] },
    { title: '路径变换', rows: [
      ['原始路径', paths?.original], ['映射路径', paths?.mapped],
      ['匹配规则编号', paths?.matched_rules?.length ? paths.matched_rules.join(', ') : null],
      ['解析路径', paths?.resolved], ['最终 URL', paths?.final_url],
    ] },
    { title: '耗时分布', rows: [
      ['最慢步骤', timing?.slowest_step], ['最慢步骤耗时', timing?.slowest_step_ms],
      ['最慢步骤占比', r.duration_ms && timing?.slowest_step_ms != null ? `${Math.min(100, timing.slowest_step_ms / r.duration_ms * 100).toFixed(1)}%` : null],
      ['未归属耗时', timing?.unaccounted_ms], ['跳转次数', timing?.redirect_count],
      ['名称解析调用耗时', timing?.name_resolution_ms], ['名称解析调用次数', timing?.name_resolution_calls],
      ['连接建立尝试合计耗时', timing?.connection_setup_ms], ['连接建立尝试次数', timing?.connection_attempts],
      ['连接观测状态', connectionObservationLabel(timing?.connection_observation)],
    ] },
  ]
})

function detailValue(label: string, value: unknown) {
  if (['剩余有效期', '最慢步骤耗时', '未归属耗时', '名称解析调用耗时', '连接建立尝试合计耗时'].includes(label)) {
    return milliseconds(value as number | null | undefined)
  }
  if (['名称解析调用次数', '连接建立尝试次数'].includes(label)) return value == null ? t('未采集') : String(value)
  if (['选择原因', '缓存原因', '错误类别'].includes(label)) return diagnosticMessage(value)
  if (label === '最慢步骤' && typeof value === 'string') return t(value)
  return detailText(value)
}

function sharePayload(records: DiagnosisRecord[]) {
  return JSON.stringify({
    scope: 'current_page', redaction: 'share_allowlist_v1',
    page: loadedPage.value, records: records.map(diagnosisShareRecord),
  }, null, 2)
}

async function copyRecord(record: DiagnosisRecord) {
  await copyText(JSON.stringify(diagnosisShareRecord(record), null, 2))
}

function exportCurrentPage() {
  if (loading.value || cleaning.value || error.value || !diagnostics.value.length) return
  const link = document.createElement('a')
  const url = URL.createObjectURL(new Blob([sharePayload(diagnostics.value)], { type: 'application/json;charset=utf-8' }))
  try {
    link.href = url
    link.download = `embypanel-diagnostics-page-${loadedPage.value ?? query.value.page}.json`
    document.body.appendChild(link)
    link.click()
  } finally {
    link.remove()
    URL.revokeObjectURL(url)
  }
}

function openDetail(record: DiagnosisRecord) {
  selectedRecord.value = record
  detailOpen.value = true
}

function onDetailOpenChange(open: boolean) {
  detailOpen.value = open
  if (!open) selectedRecord.value = null
}

function viewLogs(view: 'proxy' | 'playback' | 'general') {
  const record = selectedRecord.value
  if (!record) return
  onDetailOpenChange(false)
  openDiagnosticLogs(record.request_id, record.server_id, view)
}

onMounted(() => { void loadDiagnostics() })
onUnmounted(dispose)
</script>

<template>
  <section class="diagnostics-page" :aria-busy="loading || cleaning">
    <header class="diagnostics-header">
      <div class="page-heading">
        <span class="heading-icon" aria-hidden="true"><GitBranch :size="21" /></span>
        <div>
          <h1>{{ t('链路诊断') }}</h1>
        </div>
      </div>
      <div class="header-actions">
        <RefreshButton
          :label="t('刷新')"
          :loading-label="t('刷新中')"
          :action="() => loadDiagnostics()"
          :loading="loading"
          :disabled="loading || cleaning"
        />
        <button
          v-if="canGlobalCleanup"
          type="button"
          class="danger-button"
          :disabled="loading || cleaning"
          @click="cleanDiagnostics"
        >
          <Trash2 :size="15" aria-hidden="true" />
          {{ cleaning ? t('处理中') : t('清空诊断记录') }}
        </button>
      </div>
    </header>

    <form class="diagnostics-filters" :aria-label="t('筛选')" @submit.prevent="applyFilters">
      <label>
        <span>{{ t('请求 ID') }}</span>
        <input v-model="query.request_id" type="text" maxlength="128" autocomplete="off" :readonly="loading || cleaning" />
      </label>
      <label>
        <span>{{ t('服务器 ID') }}</span>
        <input v-model="query.server_id" type="text" maxlength="128" autocomplete="off" :readonly="loading || cleaning" />
      </label>
      <label>
        <span>{{ t('媒体 ID') }}</span>
        <input v-model="query.media_id" type="text" maxlength="128" autocomplete="off" :readonly="loading || cleaning" />
      </label>
      <label>
        <span>{{ t('处理结果') }}</span>
        <select v-model="query.outcome" :disabled="loading || cleaning">
          <option value="">{{ t('全部') }}</option>
          <option v-for="option in outcomeOptions" :key="option.value" :value="option.value">{{ t(option.label) }}</option>
        </select>
      </label>
      <label><span>{{ t('用户') }}</span><input v-model="query.user" type="text" maxlength="256" :readonly="loading || cleaning" /></label>
      <label><span>{{ t('关键词') }}</span><input v-model="query.keyword" type="text" maxlength="256" :readonly="loading || cleaning" /></label>
      <label><span>{{ t('开始时间（本地时区）') }}</span><input v-model="query.since" type="datetime-local" step="0.001" :readonly="loading || cleaning" /></label>
      <label><span>{{ t('结束时间（本地时区）') }}</span><input v-model="query.until" type="datetime-local" step="0.001" :readonly="loading || cleaning" /></label>
      <label><span>{{ t('最小耗时（毫秒）') }}</span><input v-model.number="query.min_duration_ms" type="number" min="0" step="1" :readonly="loading || cleaning" /></label>
      <label class="number-field">
        <span>{{ t('每页') }}</span>
        <input v-model.number="query.limit" type="number" min="1" max="100" :readonly="loading || cleaning" />
      </label>
      <label class="number-field">
        <span>{{ t('页码') }}</span>
        <input v-model.number="query.page" type="number" min="1" :readonly="loading || cleaning" />
      </label>
      <div class="filter-actions">
        <button type="submit" class="filter-submit" :disabled="loading || cleaning">
          <Search :size="15" aria-hidden="true" />{{ t('筛选') }}
        </button>
        <button type="button" class="secondary" :disabled="loading || cleaning" @click="resetFilters">
          <RotateCcw :size="14" aria-hidden="true" />{{ t('重置') }}
        </button>
      </div>
    </form>

    <div class="storage-summary" role="status">
      <span v-if="storage">{{ t(storage.persistent ? '持久化已启用' : '仅内存记录') }} · {{ t('保留天数') }}: {{ storage.retention_days }} · {{ t('持久化条数上限') }}: {{ storage.max_records }} · {{ t('内存容量') }}: {{ storage.memory_capacity }}</span>
      <span v-else>{{ t('存储状态未提供') }}</span>
      <p>{{ t('分享复制与导出仅含安全字段，移除用户、设备、IP、路径及自由文本；授权详情保留后端脱敏的排障信息。') }}</p>
      <button type="button" class="secondary" :disabled="loading || cleaning || !!error || !diagnostics.length" @click="exportCurrentPage">{{ t('导出当前页（脱敏 JSON）') }}</button>
      <span>{{ t('仅导出当前已加载页，不含全部历史或筛选条件。') }}</span>
    </div>

    <section class="diagnostics-results" aria-labelledby="diagnostics-results-title">
      <header class="results-header">
        <div class="results-heading">
          <h2 id="diagnostics-results-title">{{ t('诊断记录') }}</h2>
          <span v-if="!loading && !error" class="record-count">{{ diagnostics.length }}</span>
          <span v-if="!loading && !error && total !== null" class="total-count">{{ t('匹配记录总数') }}: {{ total }}</span>
        </div>
        <div v-if="!loading && !error && diagnostics.length" class="results-summary">
          <span>{{ t('本页缓存命中') }} <strong>{{ pageSummary.hits }}</strong></span>
          <span :class="{ 'has-issues': pageSummary.issues > 0 }">{{ t('本页异常') }} <strong>{{ pageSummary.issues }}</strong></span>
        </div>
      </header>

      <div v-if="loading" class="diagnostics-state" role="status" aria-live="polite">
        <RefreshCw :size="23" class="spinning" aria-hidden="true" />
        <span>{{ t('加载中') }}</span>
      </div>
      <div v-else-if="error" class="diagnostics-state is-error" role="alert">
        <TriangleAlert :size="24" aria-hidden="true" />
        <span>{{ error }}</span>
        <button type="button" class="secondary" :disabled="cleaning" @click="() => loadDiagnostics()">{{ t('重试') }}</button>
      </div>
      <div v-else-if="!diagnostics.length" class="diagnostics-state">
        <GitBranch :size="28" aria-hidden="true" />
        <span>{{ t('暂无诊断记录') }}</span>
      </div>
      <div v-else class="record-list">
        <article v-for="record in diagnostics" :key="record.request_id" class="diagnostic-record" :class="{ 'has-issue': hasIssue(record), 'is-cancelled': isCancelled(record) }">
          <div class="record-header">
            <div class="record-title">
              <button type="button" class="request-link" :aria-label="`${t('查看详情')} ${record.request_id}`" @click="openDetail(record)">
                <code>{{ record.request_id }}</code>
              </button>
              <span class="status-badge" :class="record.cache_hit ? 'is-hit' : 'is-neutral'">
                <span class="cache-state-dot" aria-hidden="true" />
                {{ cacheLabel(record) }}
              </span>
              <span class="status-badge is-neutral">{{ outcomeLabel(record) }}</span>
              <span v-if="hasIssue(record)" class="status-badge is-failed" :class="{ 'is-cancelled': isCancelled(record) }">
                <TriangleAlert :size="12" aria-hidden="true" />
                {{ issueLabel(record) }}
              </span>
            </div>
            <button type="button" class="detail-button secondary" :aria-label="`${t('查看详情')} ${record.request_id}`" @click="openDetail(record)">
              {{ t('详情') }}<ChevronRight :size="14" aria-hidden="true" />
            </button>
          </div>

          <div class="record-timing">
            <span><Clock3 :size="13" aria-hidden="true" />{{ t('开始时间') }}: <strong>{{ diagnosisTime(record.started_at_ms) }}</strong></span>
            <span><Timer :size="13" aria-hidden="true" />{{ t('诊断总耗时') }}: <strong>{{ record.duration_ms == null ? '—' : `${record.duration_ms} ms` }}</strong></span>
          </div>

          <div class="record-meta">
            <span class="meta-server" :title="record.server_id">
              <Server :size="13" aria-hidden="true" /><span>{{ t('服务器') }}: <code>{{ record.context?.server_name || record.server_id || '—' }}</code></span>
            </span>
            <span class="meta-media" :title="record.media_id">
              <Film :size="13" aria-hidden="true" /><span>{{ t(record.context?.media_name ? '媒体' : '媒体 ID') }}: <code>{{ record.context?.media_name || record.media_id || '—' }}</code></span>
            </span>
            <span class="meta-client">
              <Monitor :size="13" aria-hidden="true" /><span>{{ record.context?.user_name || '—' }} · {{ record.context?.client_name || '—' }}</span>
            </span>
          </div>

          <ol v-if="record.steps.length" class="step-chain" :aria-label="t('诊断步骤')">
            <li v-for="(step, index) in record.steps" :key="`${index}-${step.name}`" class="chain-step" :class="stepState(record, step)">
              <span class="step-dot" aria-hidden="true" />
              <span class="step-name">{{ t(step.name) }}</span>
              <span v-if="stepState(record, step) === 'skipped'" class="step-duration">{{ t('已跳过') }}</span>
              <span v-else class="step-duration">{{ step.duration_ms }} <span class="duration-unit">ms</span></span>
              <span v-if="stepState(record, step) !== 'skipped'" class="sr-only">{{ stepStateLabel(record, step) }}</span>
            </li>
          </ol>
          <p v-else class="no-steps">{{ t('暂无诊断步骤') }}</p>
          <p v-if="record.sanitized_errors.length" class="record-error">
            {{ issueLabel(record) }}
          </p>
        </article>
      </div>

      <footer v-if="!loading && !error" class="diagnostics-pager">
        <span>{{ t('页码') }} <strong>{{ loadedPage ?? query.page }}</strong> <span class="pager-divider">/</span> {{ t('本页记录') }} {{ diagnostics.length }}</span>
        <div class="pager-actions">
          <button type="button" class="secondary" :disabled="query.page <= 1 || cleaning" @click="prevPage">
            <ChevronLeft :size="14" aria-hidden="true" />{{ t('上一页') }}
          </button>
          <button type="button" class="secondary" :disabled="!hasNextPage || cleaning" @click="nextPage">
            {{ t('下一页') }}<ChevronRight :size="14" aria-hidden="true" />
          </button>
        </div>
      </footer>
    </section>

    <SettingsDialogShell
      :open="detailOpen"
      :title="t('诊断详情')"
      :description="selectedRecord?.request_id || t('诊断步骤')"
      :close-label="t('关闭')"
      :show-description="true"
      content-class="diagnostics-detail-dialog"
      @update:open="onDetailOpenChange"
    >
      <template #icon><GitBranch :size="20" aria-hidden="true" /></template>
      <div v-if="selectedRecord" class="diagnostics-detail">
        <dl class="detail-summary">
          <div><dt>{{ t('请求 ID') }}</dt><dd><code>{{ selectedRecord.request_id }}</code></dd></div>
          <div><dt>{{ t('开始时间') }}</dt><dd>{{ diagnosisTime(selectedRecord.started_at_ms) }}</dd></div>
          <div><dt>{{ t('结束时间') }}</dt><dd>{{ diagnosisTime(selectedRecord.finished_at_ms) }}</dd></div>
          <div><dt>{{ t('开始时间戳（毫秒）') }}</dt><dd><code>{{ selectedRecord.started_at_ms ?? '—' }}</code></dd></div>
          <div><dt>{{ t('结束时间戳（毫秒）') }}</dt><dd><code>{{ selectedRecord.finished_at_ms ?? '—' }}</code></dd></div>
          <div><dt>{{ t('诊断总耗时') }}</dt><dd>{{ selectedRecord.duration_ms == null ? '—' : `${selectedRecord.duration_ms} ms` }}</dd></div>
          <div><dt>{{ t('服务器 ID') }}</dt><dd><code>{{ detailText(selectedRecord.server_id) }}</code></dd></div>
          <div><dt>{{ t('媒体 ID') }}</dt><dd><code>{{ detailText(selectedRecord.media_id) }}</code></dd></div>
          <div class="detail-ua"><dt>{{ t('用户代理') }}</dt><dd>{{ detailText(selectedRecord.ua) }}</dd></div>
          <div><dt>{{ t('缓存') }}</dt><dd :class="{ 'cache-hit-text': selectedRecord.cache_hit }">{{ cacheLabel(selectedRecord) }}</dd></div>
        </dl>

        <div class="detail-actions">
          <button type="button" class="secondary" @click="copyText(selectedRecord.request_id)">{{ t('复制关联 ID') }}</button>
          <button type="button" class="secondary" @click="copyRecord(selectedRecord)">{{ t('复制记录（脱敏 JSON）') }}</button>
        </div>
        <p class="timing-help">{{ t('302 仅表示已返回跳转，不代表播放成功；代理结果仅覆盖处理和响应头，不覆盖整个媒体流。') }}</p>
        <section v-for="section in detailSections" :key="section.title" class="detail-section">
          <h3>{{ t(section.title) }}</h3>
          <dl class="detail-summary">
            <div v-for="row in section.rows" :key="String(row[0])"><dt>{{ t(String(row[0])) }}</dt><dd>{{ detailValue(String(row[0]), row[1]) }}</dd></div>
          </dl>
          <template v-if="section.title === '耗时分布'">
            <p class="timing-help">{{ t('名称解析调用耗时是系统名称解析 API 的等待时间，包含排队和操作系统 resolver 等开销，不是线上 DNS 报文耗时。') }}</p>
            <p class="timing-help">{{ t('连接建立尝试合计耗时可能包含代理、CONNECT 和 TLS，不是纯 TCP/TLS 耗时；受 hyper 连接池归因限制，观测到的 attempt 可能不能归因于当前 HTTP 响应，也不能证明该响应使用了该连接。') }}</p>
            <p class="timing-help">{{ t('名称解析与连接建立尝试耗时可能重叠，不可相加，也不额外计入诊断总耗时或未归属耗时。未归属耗时是未被操作区间覆盖的诊断时间，不等于网络耗时。无 resolver 或连接事件不代表连接复用成功；0 是实际观测值，空值为未采集，缺失观测状态为未知。') }}</p>
          </template>
        </section>
        <section class="detail-section">
          <h3>{{ t('诊断步骤') }}<span>{{ selectedRecord.steps.length }}</span></h3>
          <p class="timing-help">{{ t('时间按浏览器本地时区显示；总耗时为诊断链路处理时长，不含后续媒体传输。') }}</p>
          <ol v-if="selectedRecord.steps.length" class="detail-timeline">
            <li v-for="(step, index) in selectedRecord.steps" :key="`${index}-${step.name}`" class="detail-step" :class="stepState(selectedRecord, step)">
              <span class="step-index" aria-hidden="true">{{ index + 1 }}</span>
              <div class="step-content">
                <div class="detail-step-head">
                  <h4>{{ detailText(t(step.name)) }}</h4>
                  <span class="detail-step-state">{{ stepStateLabel(selectedRecord, step) }}</span>
                  <span class="detail-step-duration">{{ step.duration_ms }} ms</span>
                </div>
                <dl class="step-timing">
                  <div><dt>{{ t('开始时间') }}</dt><dd>{{ diagnosisTime(step.started_at_ms) }}</dd></div>
                  <div><dt>{{ t('结束时间') }}</dt><dd>{{ diagnosisTime(step.finished_at_ms) }}</dd></div>
                </dl>
                <p v-if="step.name === '缓存' && stepState(selectedRecord, step) !== 'skipped'" class="timing-help">{{ t('缓存节点可能合并多次操作；起止时间覆盖这些操作，耗时仅累计缓存操作本身。') }}</p>
                <dl class="step-observations">
                  <div><dt>{{ t('选择原因') }}</dt><dd>{{ diagnosticMessage(step.reason) }}</dd></div>
                  <div><dt>{{ t('错误类别') }}</dt><dd>{{ diagnosticMessage(step.error_kind) }}</dd></div>
                  <div><dt>{{ t('上游状态码') }}</dt><dd>{{ step.upstream_status ?? '—' }}</dd></div>
                  <div><dt>{{ t('响应头等待耗时') }}</dt><dd>{{ milliseconds(step.response_headers_ms) }}</dd></div>
                  <div><dt>{{ t('响应体读取解析耗时') }}</dt><dd>{{ milliseconds(step.response_body_ms) }}</dd></div>
                  <div><dt>{{ t('名称解析调用耗时') }}</dt><dd>{{ milliseconds(step.name_resolution_ms) }}</dd></div>
                  <div><dt>{{ t('名称解析调用次数') }}</dt><dd>{{ step.name_resolution_calls ?? t('未采集') }}</dd></div>
                  <div><dt>{{ t('连接建立尝试合计耗时') }}</dt><dd>{{ milliseconds(step.connection_setup_ms) }}</dd></div>
                  <div><dt>{{ t('连接建立尝试次数') }}</dt><dd>{{ step.connection_attempts ?? t('未采集') }}</dd></div>
                  <div><dt>{{ t('连接观测状态') }}</dt><dd>{{ connectionObservationLabel(step.connection_observation) }}</dd></div>
                </dl>
                <p class="timing-help">{{ t('响应头等待包含请求至收到响应头的时间，不是纯网络握手或播放器首帧耗时。') }}</p>
                <p class="step-details">{{ detailText(t(step.details)) }}</p>
              </div>
            </li>
          </ol>
          <p v-else class="no-steps">{{ t('暂无诊断步骤') }}</p>
        </section>

        <section v-if="hasIssue(selectedRecord)" class="detail-errors" :class="{ 'is-cancelled': isCancelled(selectedRecord) }">
          <h3><TriangleAlert :size="15" aria-hidden="true" />{{ t('异常信息') }}</h3>
          <p v-if="selectedRecord.failure_node">{{ detailText(issueLabel(selectedRecord)) }}</p>
          <ul v-if="selectedRecord.sanitized_errors.length">
            <li v-for="(item, index) in selectedRecord.sanitized_errors" :key="index">{{ detailText(t(item)) }}</li>
          </ul>
        </section>
      </div>
      <template #footer>
        <button v-if="selectedRecord" type="button" class="secondary" @click="viewLogs('proxy')">{{ t('查看请求日志') }}</button>
        <button v-if="selectedRecord" type="button" class="secondary" @click="viewLogs(selectedRecord.result?.outcome === 'redirect' ? 'playback' : 'general')">{{ t(selectedRecord.result?.outcome === 'redirect' ? '查看播放日志' : '查看运行日志') }}</button>
        <button type="button" class="secondary" @click="onDetailOpenChange(false)">{{ t('关闭') }}</button>
      </template>
    </SettingsDialogShell>
  </section>
</template>

<style scoped>
.diagnostics-page {
  display: grid;
  gap: 18px;
  min-width: 0;
  padding-bottom: 24px;
}

.diagnostics-header,
.page-heading,
.header-actions,
.filter-actions,
.results-header,
.results-heading,
.results-summary,
.record-header,
.record-title,
.status-badge,
.record-meta,
.record-meta > span,
.diagnostics-pager,
.pager-actions,
.detail-step-head {
  display: flex;
  align-items: center;
}

.diagnostics-header,
.results-header,
.record-header,
.diagnostics-pager {
  justify-content: space-between;
  gap: 12px;
}

.page-heading { gap: 12px; min-width: 0; }
.heading-icon {
  display: grid;
  place-items: center;
  width: 42px;
  height: 42px;
  flex-shrink: 0;
  border: 1px solid color-mix(in srgb, var(--brand) 20%, var(--border));
  border-radius: 12px;
  color: var(--brand);
  background: color-mix(in srgb, var(--brand) 7%, var(--panel-bg));
}
.page-heading h1 { margin: 0; font-size: var(--font-size-page); font-weight: 700; line-height: 1.3; letter-spacing: -0.5px; }
.header-actions,
.filter-actions,
.pager-actions { gap: 8px; flex-shrink: 0; }
.diagnostics-page button:not(.refresh-button) { display: inline-flex; align-items: center; justify-content: center; gap: 7px; font-size: var(--font-size-control); font-weight: 600; }
.diagnostics-page button:focus-visible { outline: 2px solid var(--brand); outline-offset: 3px; }

.diagnostics-filters {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  align-items: end;
  gap: 12px;
  padding: 14px 16px;
  border: 1px solid var(--border);
  border-radius: 12px;
  background: var(--panel-bg);
}
.diagnostics-filters label { display: grid; gap: 6px; min-width: 0; }
.diagnostics-filters label > span { color: var(--muted-strong); font-size: var(--font-size-label); font-weight: 700; }
.diagnostics-filters input, .diagnostics-filters select { width: 100%; min-width: 0; height: 34px; padding: 0 10px; border: 1px solid var(--border); border-radius: 7px; background: var(--app-bg); color: var(--text); font-size: var(--font-size-body); font-weight: 400; }
.diagnostics-filters input:focus, .diagnostics-filters select:focus { border-color: var(--brand); outline: 2px solid color-mix(in srgb, var(--brand) 16%, transparent); }
.filter-submit { color: var(--panel-bg); background: var(--primary); }
.filter-submit:hover:not(:disabled) { opacity: 0.9; }

.diagnostics-results { min-width: 0; overflow: hidden; border: 1px solid var(--border); border-radius: 12px; background: var(--panel-bg); box-shadow: 0 4px 18px var(--shadow-soft); }
.results-header { min-height: 58px; padding: 14px 18px; flex-wrap: wrap; border-bottom: 1px solid var(--border-soft); }
.results-heading { gap: 9px; flex-wrap: wrap; }
.results-heading h2 { margin: 0; font-size: var(--font-size-section); font-weight: 700; }
.record-count { min-width: 24px; padding: 2px 7px; border-radius: 6px; background: var(--button-bg); color: var(--muted-strong); text-align: center; font: 11px ui-monospace, monospace; }
.results-summary { gap: 18px; color: var(--muted); font-size: var(--font-size-meta); }
.results-summary strong { margin-left: 5px; color: var(--text); font-variant-numeric: tabular-nums; }
.results-summary .has-issues,
.results-summary .has-issues strong { color: var(--danger-text); }
.diagnostics-state { display: grid; justify-items: center; gap: 12px; padding: 52px 20px; color: var(--muted); font-size: var(--font-size-body); overflow-wrap: anywhere; }
.diagnostics-state.is-error { color: var(--danger-text); }

.diagnostic-record { min-width: 0; padding: 15px 18px; border-bottom: 1px solid var(--border-soft); transition: background 160ms ease; }
.diagnostic-record:last-child { border-bottom: 0; }
.diagnostic-record:hover { background: color-mix(in srgb, var(--brand) 3%, var(--panel-bg)); }
.diagnostic-record.has-issue { box-shadow: inset 3px 0 var(--danger-text); }
.diagnostic-record.is-cancelled { box-shadow: inset 3px 0 var(--warn-text); }
.diagnostic-record.is-cancelled .record-error { color: var(--warn-text); }
.record-title { min-width: 0; gap: 9px; flex-wrap: wrap; }
.diagnostics-page .request-link { min-width: 0; max-width: 100%; height: auto; padding: 2px 0; border-radius: 3px; background: transparent; color: var(--text); font-weight: 600; }
.request-link code { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: var(--font-size-body); }
.request-link:hover { color: var(--brand); }
.status-badge { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; gap: 5px; padding: 3px 7px; border: 1px solid var(--border-soft); border-radius: 5px; font-size: var(--font-size-meta); line-height: 1.4; }
.status-badge.is-neutral { color: var(--muted); background: var(--subtle-bg); }
.status-badge.is-hit { border-color: color-mix(in srgb, var(--success-text) 16%, var(--border-soft)); color: var(--success-text); background: var(--success-bg); }
.status-badge.is-failed { color: var(--danger-text); background: var(--danger-bg); border-color: var(--danger-border); }
.status-badge.is-cancelled { color: var(--warn-text); background: var(--warn-bg); border-color: color-mix(in srgb, var(--warn-text) 24%, var(--border)); }
.cache-state-dot { width: 5px; height: 5px; border-radius: 50%; background: currentColor; }
.diagnostics-page .detail-button { flex-shrink: 0; height: 29px; padding: 0 8px; }
.record-timing { display: flex; flex-wrap: wrap; gap: 6px 20px; margin-top: 8px; color: var(--muted); font-size: var(--font-size-meta); }
.record-timing > span { display: inline-flex; align-items: baseline; gap: 6px; }
.record-timing svg { flex-shrink: 0; align-self: center; }
.record-timing strong { color: var(--text-soft); font-weight: 600; font-variant-numeric: tabular-nums; }
.record-meta { gap: 18px; margin-top: 7px; color: var(--muted); font-size: var(--font-size-meta); min-width: 0; }
.record-meta > span { min-width: 0; gap: 6px; }
.record-meta > span > svg { flex-shrink: 0; opacity: 0.75; }
.record-meta > span > span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.meta-server { flex: 0 1 38%; }
.meta-media { flex: 0 1 20%; }
.meta-client { flex: 1; }
.record-meta code { font-size: var(--font-size-meta); }

.step-chain { display: flex; flex-wrap: wrap; gap: 6px; margin: 11px 0 0; padding: 0; list-style: none; }
.chain-step { display: inline-flex; align-items: center; gap: 6px; min-width: 0; padding: 5px 9px; border: 1px solid var(--border-soft); border-radius: 6px; background: var(--subtle-bg); font-size: var(--font-size-meta); line-height: 1.4; }
.step-dot { flex-shrink: 0; width: 5px; height: 5px; border-radius: 50%; background: var(--brand); }
.step-name { color: var(--text-soft); overflow-wrap: anywhere; }
.step-duration { color: var(--text); font-variant-numeric: tabular-nums; white-space: nowrap; font-size: var(--font-size-meta); }
.duration-unit { color: var(--muted); }
.chain-step.skipped { border-style: dashed; background: transparent; }
.chain-step.skipped .step-dot { background: var(--muted); }
.chain-step.skipped .step-name,
.chain-step.skipped .step-duration { color: var(--muted); }
.chain-step.failed { border-color: var(--danger-border); background: var(--danger-bg); }
.chain-step.failed .step-dot { background: var(--danger-text); }
.chain-step.failed .step-name,
.chain-step.failed .step-duration { color: var(--danger-text); }
.chain-step.cancelled { border-color: color-mix(in srgb, var(--warn-text) 24%, var(--border)); background: var(--warn-bg); }
.chain-step.cancelled .step-dot { background: var(--warn-text); }
.chain-step.cancelled .step-name,
.chain-step.cancelled .step-duration { color: var(--warn-text); }
.no-steps { margin: 10px 0 0; color: var(--muted); font-size: var(--font-size-help); }
.record-error { margin: 9px 0 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--danger-text); font-size: var(--font-size-help); }
.diagnostics-pager { min-height: 60px; padding: 12px 18px; border-top: 1px solid var(--border-soft); color: var(--muted); font-size: var(--font-size-meta); }
.diagnostics-pager strong { margin-left: 3px; color: var(--text); }
.pager-divider { margin: 0 8px; color: var(--border-strong); }
.pager-actions button { height: 31px; }

:global(.settings-dialog-content.diagnostics-detail-dialog) { width: min(880px, calc(100vw - 32px)); }
:global(.diagnostics-detail-dialog .settings-dialog-description) { overflow-wrap: anywhere; }
:global(.diagnostics-detail-dialog .settings-dialog-footer) { flex-wrap: wrap; }
.diagnostics-detail { line-height: 1.6; padding: 20px; min-width: 0; }
.detail-summary { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 16px; margin: 0 0 24px; padding-bottom: 18px; border-bottom: 1px solid var(--border-soft); }
.detail-summary > div { min-width: 0; }
.detail-summary dt { margin-bottom: 6px; color: var(--muted); font-size: var(--font-size-label); font-weight: 700; }
.detail-summary dd { margin: 0; color: var(--text); font-size: var(--font-size-body); overflow-wrap: anywhere; }
.detail-ua { grid-column: span 2; }
.detail-summary .cache-hit-text { color: var(--success-text); }
.detail-section h3 { display: flex; align-items: center; gap: 8px; margin: 0 0 18px; font-size: var(--font-size-section); font-weight: 700; }
.detail-section h3 > span { color: var(--muted); font-size: var(--font-size-meta); font-weight: 400; }
.detail-timeline { margin: 0; padding: 0; list-style: none; }
.detail-step { position: relative; display: grid; grid-template-columns: 26px minmax(0, 1fr); gap: 12px; padding-bottom: 19px; }
.detail-step:not(:last-child)::before { content: ''; position: absolute; top: 26px; bottom: 0; left: 12px; border-left: 1px solid var(--border); }
.step-index { display: grid; place-items: center; width: 26px; height: 26px; border: 1px solid var(--border); border-radius: 50%; color: var(--brand); background: var(--chip-bg); font: 11px ui-monospace, monospace; }
.detail-step.failed .step-index { color: var(--danger-text); background: var(--danger-bg); border-color: var(--danger-border); }
.detail-step.skipped .step-index { color: var(--muted); background: var(--subtle-bg); border-style: dashed; }
.detail-step.cancelled .step-index { color: var(--warn-text); background: var(--warn-bg); }
.detail-step-head { gap: 9px; min-height: 26px; flex-wrap: wrap; }
.detail-step-head h4 { margin: 0; font-size: var(--font-size-body); font-weight: 600; }
.detail-step-state { color: var(--muted); font-size: var(--font-size-meta); }
.detail-step.failed .detail-step-state { color: var(--danger-text); }
.detail-step.cancelled .detail-step-state { color: var(--warn-text); }
.detail-step-duration { margin-left: auto; color: var(--text-soft); font: 11px ui-monospace, monospace; }
.step-timing { display: flex; flex-wrap: wrap; gap: 6px 20px; margin: 7px 0 0; color: var(--muted); font-size: var(--font-size-meta); }
.step-timing > div { display: flex; flex-wrap: wrap; gap: 6px; }
.step-timing dd { margin: 0; color: var(--text-soft); font-variant-numeric: tabular-nums; }
.timing-help { margin: 8px 0 12px; color: var(--muted); font-size: var(--font-size-help); line-height: 1.6; }
.step-details { margin: 7px 0 0; padding: 10px 12px; border: 1px solid var(--border-soft); border-radius: 7px; background: var(--subtle-bg); color: var(--text-soft); font: 11px/1.8 ui-monospace, monospace; white-space: pre-wrap; overflow-wrap: anywhere; }
.detail-step.failed .step-details { border-color: var(--danger-border); }
.detail-errors { padding: 14px; border: 1px solid var(--danger-border); border-radius: 8px; color: var(--danger-text); background: var(--danger-bg); font-size: var(--font-size-body); overflow-wrap: anywhere; }
.detail-errors h3 { display: flex; align-items: center; gap: 6px; margin: 0 0 8px; font-size: var(--font-size-section); font-weight: 700; }
.detail-errors p { margin: 6px 0; }
.detail-errors ul { margin: 8px 0 0; padding-left: 18px; white-space: pre-wrap; }
.detail-errors.is-cancelled { color: var(--warn-text); background: var(--warn-bg); border-color: color-mix(in srgb, var(--warn-text) 24%, var(--border)); }
.storage-summary { color: var(--muted); font-size: var(--font-size-help); line-height: 1.6; overflow-wrap: anywhere; }
.storage-summary p { margin: 6px 0; }
.storage-summary button { margin-right: 8px; }
.total-count { color: var(--muted); font-size: var(--font-size-meta); }
.detail-actions { display: flex; flex-wrap: wrap; gap: 8px; }
.detail-actions button { font-size: var(--font-size-control); }
.diagnostics-detail button:focus-visible { outline: 2px solid var(--brand); outline-offset: 3px; }
.detail-section { margin-top: 20px; }
.step-observations { display: flex; flex-wrap: wrap; gap: 8px 20px; font-size: var(--font-size-meta); color: var(--muted); }
.step-observations > div { display: flex; gap: 6px; overflow-wrap: anywhere; }
.step-observations dd { margin: 0; color: var(--text-soft); }
.spinning { animation: diagnostics-spin 1s linear infinite; }
@keyframes diagnostics-spin { to { transform: rotate(360deg); } }

@media (max-width: 1100px) {
  .diagnostics-filters { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .filter-actions { justify-content: flex-end; }
  .number-field { max-width: none; }
}
@media (max-width: 700px) {
  .diagnostics-filters input, .diagnostics-filters select { font-size: 16px; }
}
@media (max-width: 640px) {
  .diagnostics-page { gap: 14px; }
  .diagnostics-header { align-items: flex-start; flex-direction: column; }
  .heading-icon { width: 38px; height: 38px; }
  .header-actions { width: 100%; }
  .diagnostics-filters { grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px 10px; padding: 14px; }
  .filter-actions { align-self: end; }
  .filter-actions button { flex: 1; padding: 0 9px; }
  .results-header { padding: 13px 14px; gap: 10px; }
  .results-summary { gap: 12px; font-size: var(--font-size-meta); }
  .diagnostic-record { padding: 14px; }
  .record-title { gap: 6px; }
  .record-meta { flex-wrap: wrap; gap: 7px 14px; }
  .meta-server { flex: 1 1 100%; }
  .meta-media { flex: 0 1 36%; }
  .meta-client { flex: 1 1 50%; }
  .step-chain { gap: 5px; }
  .chain-step { padding: 5px 7px; font-size: var(--font-size-meta); }
  .diagnostics-pager { padding: 12px 14px; gap: 10px; flex-wrap: wrap; }
  .pager-actions { margin-left: auto; }
  .pager-actions button { padding: 0 9px; }
  .diagnostics-detail { padding: 16px; }
  .detail-summary { grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px; }
  .detail-ua { grid-column: span 2; }
}
@media (prefers-reduced-motion: reduce) {
  .spinning { animation: none; }
  .diagnostic-record { transition: none; }
}
</style>
