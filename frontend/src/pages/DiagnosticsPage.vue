<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { ChevronLeft, ChevronRight, Film, GitBranch, Monitor, RefreshCw, RotateCcw, Search, Server, Trash2, TriangleAlert } from '@lucide/vue'
import { useDiagnosticsController, type DiagnosisRecord, type DiagnosisStep } from '@/composables/useDiagnosticsController'
import { usePanelContext } from '@/composables/panel-context'
import SettingsDialogShell from '@/components/ui/SettingsDialogShell.vue'

const { t, showNotice, clearNotice, api, confirmAction } = usePanelContext()
const controller = useDiagnosticsController({ api, translate: t, showNotice, clearNotice, confirmAction })
const {
  diagnostics, loading, cleaning, canGlobalCleanup, error, query, hasNextPage,
  loadDiagnostics, applyFilters, resetFilters, nextPage, prevPage, cleanDiagnostics, dispose,
} = controller

const selectedRecord = ref<DiagnosisRecord | null>(null)
const detailOpen = ref(false)
const pageSummary = computed(() => ({
  hits: diagnostics.value.filter((record) => record.cache_hit).length,
  issues: diagnostics.value.filter(hasIssue).length,
}))

function hasIssue(record: DiagnosisRecord) {
  return Boolean(record.failure_node || record.sanitized_errors.length)
}

function isCancelled(record: DiagnosisRecord) {
  return record.steps.some((step) => step.name === record.failure_node
    && (step.details.startsWith('cancelled:') || step.details === 'cancelled before step started'))
}

function issueLabel(record: DiagnosisRecord) {
  if (!record.failure_node) return t('异常')
  return `${t(isCancelled(record) ? '已取消' : '失败节点')}: ${t(record.failure_node)}`
}

function stepState(record: DiagnosisRecord, step: DiagnosisStep) {
  if (record.failure_node === step.name) return isCancelled(record) ? 'cancelled' : 'failed'
  return step.details === 'bypassed' ? 'skipped' : 'recorded'
}

function stepStateLabel(record: DiagnosisRecord, step: DiagnosisStep) {
  const state = stepState(record, step)
  if (state === 'cancelled') return t('已取消')
  return state === 'failed' ? t('异常') : state === 'skipped' ? t('已跳过') : t('已记录')
}

function openDetail(record: DiagnosisRecord) {
  selectedRecord.value = record
  detailOpen.value = true
}

function onDetailOpenChange(open: boolean) {
  detailOpen.value = open
  if (!open) selectedRecord.value = null
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
        <button type="button" class="secondary" :disabled="loading || cleaning" @click="() => loadDiagnostics()">
          <RefreshCw :size="15" :class="{ spinning: loading }" aria-hidden="true" />
          {{ loading ? t('加载中') : t('刷新') }}
        </button>
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
        <input v-model="query.request_id" type="text" autocomplete="off" :readonly="loading || cleaning" />
      </label>
      <label>
        <span>{{ t('服务器 ID') }}</span>
        <input v-model="query.server_id" type="text" autocomplete="off" :readonly="loading || cleaning" />
      </label>
      <label>
        <span>{{ t('媒体 ID') }}</span>
        <input v-model="query.media_id" type="text" autocomplete="off" :readonly="loading || cleaning" />
      </label>
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

    <section class="diagnostics-results" aria-labelledby="diagnostics-results-title">
      <header class="results-header">
        <div class="results-heading">
          <h2 id="diagnostics-results-title">{{ t('诊断记录') }}</h2>
          <span v-if="!loading && !error" class="record-count">{{ diagnostics.length }}</span>
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
                {{ record.cache_hit ? t('缓存命中') : t('未命中') }}
              </span>
              <span v-if="hasIssue(record)" class="status-badge is-failed" :class="{ 'is-cancelled': isCancelled(record) }">
                <TriangleAlert :size="12" aria-hidden="true" />
                {{ issueLabel(record) }}
              </span>
            </div>
            <button type="button" class="detail-button secondary" :aria-label="`${t('查看详情')} ${record.request_id}`" @click="openDetail(record)">
              {{ t('详情') }}<ChevronRight :size="14" aria-hidden="true" />
            </button>
          </div>

          <div class="record-meta">
            <span class="meta-server" :title="record.server_id">
              <Server :size="13" aria-hidden="true" /><span>{{ t('服务器 ID') }}: <code>{{ record.server_id || '—' }}</code></span>
            </span>
            <span class="meta-media" :title="record.media_id">
              <Film :size="13" aria-hidden="true" /><span>{{ t('媒体 ID') }}: <code>{{ record.media_id || '—' }}</code></span>
            </span>
            <span class="meta-client" :title="record.ua">
              <Monitor :size="13" aria-hidden="true" /><span>{{ record.ua || '—' }}</span>
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
        <span>{{ t('页码') }} <strong>{{ query.page }}</strong> <span class="pager-divider">/</span> {{ t('本页记录') }} {{ diagnostics.length }}</span>
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
          <div><dt>{{ t('服务器 ID') }}</dt><dd><code>{{ selectedRecord.server_id || '—' }}</code></dd></div>
          <div><dt>{{ t('媒体 ID') }}</dt><dd><code>{{ selectedRecord.media_id || '—' }}</code></dd></div>
          <div class="detail-ua"><dt>{{ t('用户代理') }}</dt><dd>{{ selectedRecord.ua || '—' }}</dd></div>
          <div><dt>{{ t('缓存') }}</dt><dd :class="{ 'cache-hit-text': selectedRecord.cache_hit }">{{ selectedRecord.cache_hit ? t('缓存命中') : t('未命中') }}</dd></div>
        </dl>

        <section class="detail-section">
          <h3>{{ t('诊断步骤') }}<span>{{ selectedRecord.steps.length }}</span></h3>
          <ol v-if="selectedRecord.steps.length" class="detail-timeline">
            <li v-for="(step, index) in selectedRecord.steps" :key="`${index}-${step.name}`" class="detail-step" :class="stepState(selectedRecord, step)">
              <span class="step-index" aria-hidden="true">{{ index + 1 }}</span>
              <div class="step-content">
                <div class="detail-step-head">
                  <h4>{{ t(step.name) }}</h4>
                  <span class="detail-step-state">{{ stepStateLabel(selectedRecord, step) }}</span>
                  <span class="detail-step-duration">{{ step.duration_ms }} ms</span>
                </div>
                <p class="step-details">{{ t(step.details) || '—' }}</p>
              </div>
            </li>
          </ol>
          <p v-else class="no-steps">{{ t('暂无诊断步骤') }}</p>
        </section>

        <section v-if="hasIssue(selectedRecord)" class="detail-errors" :class="{ 'is-cancelled': isCancelled(selectedRecord) }">
          <h3><TriangleAlert :size="15" aria-hidden="true" />{{ t('异常信息') }}</h3>
          <p v-if="selectedRecord.failure_node">{{ issueLabel(selectedRecord) }}</p>
          <ul v-if="selectedRecord.sanitized_errors.length">
            <li v-for="(item, index) in selectedRecord.sanitized_errors" :key="index">{{ t(item) }}</li>
          </ul>
        </section>
      </div>
      <template #footer>
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
.page-heading h1 { margin: 0; font-size: 21px; font-weight: 650; letter-spacing: -0.5px; }
.header-actions,
.filter-actions,
.pager-actions { gap: 8px; flex-shrink: 0; }
.diagnostics-page button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; font-size: 12px; }
.diagnostics-page button:focus-visible { outline: 2px solid var(--brand); outline-offset: 3px; }

.diagnostics-filters {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr)) 76px 76px auto;
  align-items: end;
  gap: 12px;
  padding: 14px 16px;
  border: 1px solid var(--border);
  border-radius: 12px;
  background: var(--panel-bg);
}
.diagnostics-filters label { display: grid; gap: 6px; min-width: 0; }
.diagnostics-filters label > span { color: var(--muted-strong); font-size: 11px; font-weight: 600; }
.diagnostics-filters input { width: 100%; min-width: 0; height: 34px; padding: 0 10px; border: 1px solid var(--border); border-radius: 7px; background: var(--app-bg); color: var(--text); font-size: 12px; }
.diagnostics-filters input:focus { border-color: var(--brand); outline: 2px solid color-mix(in srgb, var(--brand) 16%, transparent); }
.filter-submit { color: var(--panel-bg); background: var(--primary); }
.filter-submit:hover:not(:disabled) { opacity: 0.9; }

.diagnostics-results { min-width: 0; overflow: hidden; border: 1px solid var(--border); border-radius: 12px; background: var(--panel-bg); box-shadow: 0 4px 18px var(--shadow-soft); }
.results-header { min-height: 58px; padding: 14px 18px; flex-wrap: wrap; border-bottom: 1px solid var(--border-soft); }
.results-heading { gap: 9px; }
.results-heading h2 { margin: 0; font-size: 14px; font-weight: 650; }
.record-count { min-width: 24px; padding: 2px 7px; border-radius: 6px; background: var(--button-bg); color: var(--muted-strong); text-align: center; font: 11px ui-monospace, monospace; }
.results-summary { gap: 18px; color: var(--muted); font-size: 11px; }
.results-summary strong { margin-left: 5px; color: var(--text); font-variant-numeric: tabular-nums; }
.results-summary .has-issues,
.results-summary .has-issues strong { color: var(--danger-text); }
.diagnostics-state { display: grid; justify-items: center; gap: 12px; padding: 52px 20px; color: var(--muted); font-size: 13px; overflow-wrap: anywhere; }
.diagnostics-state.is-error { color: var(--danger-text); }

.diagnostic-record { min-width: 0; padding: 15px 18px; border-bottom: 1px solid var(--border-soft); transition: background 160ms ease; }
.diagnostic-record:last-child { border-bottom: 0; }
.diagnostic-record:hover { background: color-mix(in srgb, var(--brand) 3%, var(--panel-bg)); }
.diagnostic-record.has-issue { box-shadow: inset 3px 0 var(--danger-text); }
.diagnostic-record.is-cancelled { box-shadow: inset 3px 0 var(--warn-text); }
.diagnostic-record.is-cancelled .record-error { color: var(--warn-text); }
.record-title { min-width: 0; gap: 9px; flex-wrap: wrap; }
.diagnostics-page .request-link { min-width: 0; max-width: 100%; height: auto; padding: 2px 0; border-radius: 3px; background: transparent; color: var(--text); font-weight: 650; }
.request-link code { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 13px; }
.request-link:hover { color: var(--brand); }
.status-badge { gap: 5px; padding: 3px 7px; border: 1px solid var(--border-soft); border-radius: 5px; font-size: 10px; line-height: 1.3; }
.status-badge.is-neutral { color: var(--muted); background: var(--subtle-bg); }
.status-badge.is-hit { border-color: color-mix(in srgb, var(--success-text) 16%, var(--border-soft)); color: var(--success-text); background: var(--success-bg); }
.status-badge.is-failed { color: var(--danger-text); background: var(--danger-bg); border-color: var(--danger-border); }
.status-badge.is-cancelled { color: var(--warn-text); background: var(--warn-bg); border-color: color-mix(in srgb, var(--warn-text) 24%, var(--border)); }
.cache-state-dot { width: 5px; height: 5px; border-radius: 50%; background: currentColor; }
.diagnostics-page .detail-button { flex-shrink: 0; height: 29px; padding: 0 8px; }
.record-meta { gap: 18px; margin-top: 7px; color: var(--muted); font-size: 11px; min-width: 0; }
.record-meta > span { min-width: 0; gap: 6px; }
.record-meta > span > svg { flex-shrink: 0; opacity: 0.75; }
.record-meta > span > span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.meta-server { flex: 0 1 38%; }
.meta-media { flex: 0 1 20%; }
.meta-client { flex: 1; }
.record-meta code { font-size: 11px; }

.step-chain { display: flex; flex-wrap: wrap; gap: 6px; margin: 11px 0 0; padding: 0; list-style: none; }
.chain-step { display: inline-flex; align-items: center; gap: 6px; min-width: 0; padding: 5px 9px; border: 1px solid var(--border-soft); border-radius: 6px; background: var(--subtle-bg); font-size: 11px; line-height: 1.4; }
.step-dot { flex-shrink: 0; width: 5px; height: 5px; border-radius: 50%; background: var(--brand); }
.step-name { color: var(--text-soft); overflow-wrap: anywhere; }
.step-duration { color: var(--text); font-variant-numeric: tabular-nums; white-space: nowrap; font-size: 10px; }
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
.no-steps { margin: 10px 0 0; color: var(--muted); font-size: 12px; }
.record-error { margin: 9px 0 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--danger-text); font-size: 11px; }
.diagnostics-pager { min-height: 60px; padding: 12px 18px; border-top: 1px solid var(--border-soft); color: var(--muted); font-size: 11px; }
.diagnostics-pager strong { margin-left: 3px; color: var(--text); }
.pager-divider { margin: 0 8px; color: var(--border-strong); }
.pager-actions button { height: 31px; }

:global(.settings-dialog-content.diagnostics-detail-dialog) { width: min(880px, calc(100vw - 32px)); }
.diagnostics-detail { padding: 20px; min-width: 0; }
.detail-summary { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 16px; margin: 0 0 24px; padding-bottom: 18px; border-bottom: 1px solid var(--border-soft); }
.detail-summary > div { min-width: 0; }
.detail-summary dt { margin-bottom: 6px; color: var(--muted); font-size: 11px; }
.detail-summary dd { margin: 0; color: var(--text); font-size: 12px; overflow-wrap: anywhere; }
.detail-ua { grid-column: span 2; }
.detail-summary .cache-hit-text { color: var(--success-text); }
.detail-section h3 { display: flex; align-items: center; gap: 8px; margin: 0 0 18px; font-size: 13px; font-weight: 650; }
.detail-section h3 > span { color: var(--muted); font-size: 11px; font-weight: 400; }
.detail-timeline { margin: 0; padding: 0; list-style: none; }
.detail-step { position: relative; display: grid; grid-template-columns: 26px minmax(0, 1fr); gap: 12px; padding-bottom: 19px; }
.detail-step:not(:last-child)::before { content: ''; position: absolute; top: 26px; bottom: 0; left: 12px; border-left: 1px solid var(--border); }
.step-index { display: grid; place-items: center; width: 26px; height: 26px; border: 1px solid var(--border); border-radius: 50%; color: var(--brand); background: var(--chip-bg); font: 11px ui-monospace, monospace; }
.detail-step.failed .step-index { color: var(--danger-text); background: var(--danger-bg); border-color: var(--danger-border); }
.detail-step.skipped .step-index { color: var(--muted); background: var(--subtle-bg); border-style: dashed; }
.detail-step.cancelled .step-index { color: var(--warn-text); background: var(--warn-bg); }
.detail-step-head { gap: 9px; min-height: 26px; flex-wrap: wrap; }
.detail-step-head h4 { margin: 0; font-size: 12px; font-weight: 650; }
.detail-step-state { color: var(--muted); font-size: 10px; }
.detail-step.failed .detail-step-state { color: var(--danger-text); }
.detail-step.cancelled .detail-step-state { color: var(--warn-text); }
.detail-step-duration { margin-left: auto; color: var(--text-soft); font: 11px ui-monospace, monospace; }
.step-details { margin: 7px 0 0; padding: 10px 12px; border: 1px solid var(--border-soft); border-radius: 7px; background: var(--subtle-bg); color: var(--text-soft); font: 11px/1.8 ui-monospace, monospace; white-space: pre-wrap; overflow-wrap: anywhere; }
.detail-step.failed .step-details { border-color: var(--danger-border); }
.detail-errors { padding: 14px; border: 1px solid var(--danger-border); border-radius: 8px; color: var(--danger-text); background: var(--danger-bg); font-size: 12px; overflow-wrap: anywhere; }
.detail-errors h3 { display: flex; align-items: center; gap: 6px; margin: 0 0 8px; font-size: 12px; }
.detail-errors p { margin: 6px 0; }
.detail-errors ul { margin: 8px 0 0; padding-left: 18px; white-space: pre-wrap; }
.detail-errors.is-cancelled { color: var(--warn-text); background: var(--warn-bg); border-color: color-mix(in srgb, var(--warn-text) 24%, var(--border)); }
.spinning { animation: diagnostics-spin 1s linear infinite; }
@keyframes diagnostics-spin { to { transform: rotate(360deg); } }

@media (max-width: 1100px) {
  .diagnostics-filters { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .filter-actions { justify-content: flex-end; }
  .number-field { max-width: none; }
}
@media (max-width: 640px) {
  .diagnostics-page { gap: 14px; }
  .diagnostics-header { align-items: flex-start; flex-direction: column; }
  .page-heading h1 { font-size: 19px; }
  .heading-icon { width: 38px; height: 38px; }
  .header-actions { width: 100%; }
  .diagnostics-filters { grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px 10px; padding: 14px; }
  .filter-actions { align-self: end; }
  .filter-actions button { flex: 1; padding: 0 9px; }
  .results-header { padding: 13px 14px; gap: 10px; }
  .results-summary { gap: 12px; font-size: 10px; }
  .diagnostic-record { padding: 14px; }
  .record-title { gap: 6px; }
  .record-meta { flex-wrap: wrap; gap: 7px 14px; }
  .meta-server { flex: 1 1 100%; }
  .meta-media { flex: 0 1 36%; }
  .meta-client { flex: 1 1 50%; }
  .step-chain { gap: 5px; }
  .chain-step { padding: 5px 7px; font-size: 10px; }
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
