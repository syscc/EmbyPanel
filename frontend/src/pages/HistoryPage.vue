<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { RefreshCw } from '@lucide/vue'
import { formatMetric, useHistoryController } from '@/composables/useHistoryController'
import { usePanelContext } from '@/composables/panel-context'
import P2LineChart from '@/components/ui/P2LineChart.vue'
import P2MetricStrip from '@/components/ui/P2MetricStrip.vue'

const panel = usePanelContext()
const { t, api, serverOptions: authorizedServers } = panel
const refreshing = ref(false)
let refreshGeneration = 0

const {
  loading,
  error,
  metric,
  dimension,
  days,
  server,
  capability,
  metricOptions,
  dimensionOptions,
  dayOptions,
  serverOptions,
  chartLabels,
  chartSeries,
  metricItems,
  chartTitle,
  chartAriaLabel,
  loadHistory,
  setMetric,
  setDimension,
  setDays,
  setServer,
  dispose,
} = useHistoryController({
  api,
  translate: t,
  servers: () =>
    authorizedServers.value.map((item) => ({
      id: item.id,
      name: item.name || item.id,
    })),
})

const chartValueFormatter = (value: number) => formatMetric(metric.value, value)


// Single request path: immediate load + filter changes. Avoids mounted+watch double fetch.
watch(
  [metric, dimension, days, server],
  () => {
    void loadHistory()
  },
  { immediate: true },
)

watch(serverOptions, (options) => {
  if (!options.some((option) => option.value === server.value)) {
    setServer('all')
  }
})

onBeforeUnmount(dispose)

async function refreshPage() {
  if (refreshing.value || loading.value) return
  const generation = ++refreshGeneration
  refreshing.value = true
  try {
    if (!await panel.refreshServerOptions()) return
    await loadHistory()
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    if (generation === refreshGeneration) refreshing.value = false
  }
}

function onMetricChange(event: Event) {
  const target = event.target as HTMLSelectElement | null
  setMetric(target?.value ?? 'requests')
}

function onDimensionChange(event: Event) {
  const target = event.target as HTMLSelectElement | null
  setDimension(target?.value ?? 'server')
}

function onDaysChange(event: Event) {
  const target = event.target as HTMLSelectElement | null
  setDays(target?.value ?? 7)
}

function onServerChange(event: Event) {
  const target = event.target as HTMLSelectElement | null
  setServer(target?.value ?? 'all')
}
</script>

<template>
  <section class="history-page">
    <header class="history-head">
      <div class="history-head-copy">
        <div class="history-kicker">
          <span class="history-kicker-dot" aria-hidden="true" />
          {{ t('历史分析') }}
        </div>
        <h1>{{ t('历史') }}</h1>
        <p class="muted">
          {{ t('按日聚合的反代请求统计趋势') }}
        </p>
      </div>
      <button
        type="button"
        class="refresh-button"
        :title="t('刷新')"
        :disabled="loading || refreshing"
        @click="refreshPage"
      >
        <RefreshCw :size="15" :class="{ spinning: loading || refreshing }" />
        {{ loading ? t('加载中') : t('刷新') }}
      </button>
    </header>

    <div class="history-filter-band" role="group" :aria-label="t('历史')">
      <div class="history-filter-intro">
        <div>
          <span class="history-filter-label">{{ t('趋势图') }}</span>
          <strong>{{ chartTitle }}</strong>
        </div>
        <span class="history-filter-state" :class="{ active: !loading && chartSeries.length > 0 }">
          {{ loading ? t('加载中') : chartSeries.length > 0 ? t('有数据') : t('无数据') }}
        </span>
      </div>

      <div class="history-filters">
        <label class="filter-field">
          <span>{{ t('指标') }}</span>
          <select :value="metric" :disabled="loading || refreshing" @change="onMetricChange">
            <option
              v-for="option in metricOptions"
              :key="option.value"
              :value="option.value"
            >
              {{ option.label }}
            </option>
          </select>
        </label>

        <label class="filter-field">
          <span>{{ t('维度') }}</span>
          <select :value="dimension" :disabled="loading || refreshing" @change="onDimensionChange">
            <option
              v-for="option in dimensionOptions"
              :key="option.value"
              :value="option.value"
            >
              {{ option.label }}
            </option>
          </select>
        </label>

        <label class="filter-field">
          <span>{{ t('时间范围') }}</span>
          <select :value="days" :disabled="loading || refreshing" @change="onDaysChange">
            <option
              v-for="option in dayOptions"
              :key="option.value"
              :value="option.value"
            >
              {{ option.label }}
            </option>
          </select>
        </label>

        <label class="filter-field">
          <span>{{ t('服务器') }}</span>
          <select :value="server" :disabled="loading || refreshing" @change="onServerChange">
            <option
              v-for="option in serverOptions"
              :key="option.value"
              :value="option.value"
            >
              {{ option.label }}
            </option>
          </select>
        </label>
      </div>
    </div>

    <div v-if="error" class="notice error" role="alert">
      <span>{{ error }}</span>
      <button type="button" class="link-button" :disabled="loading || refreshing" @click="refreshPage">
        {{ t('重试') }}
      </button>
    </div>

    <div v-if="!capability.available || capability.reason || capability.caveat" class="notice capability" role="status">
      <span>
        {{ capability.available ? (capability.reason || capability.caveat || '').split('；').map((part) => t(part)).join(' · ') : `${t('不可用')}: ${capability.reason ? t(capability.reason) : t('暂无数据')}` }}
      </span>
    </div>

    <P2MetricStrip
      :items="metricItems"
      :aria-label="chartTitle"
      :empty-label="t('无数据')"
    />

    <div class="chart-panel">
      <P2LineChart
        :title="chartTitle"
        :labels="chartLabels"
        :series="chartSeries"
        :value-formatter="chartValueFormatter"
        :empty-label="loading ? t('加载中') : t('无数据')"
        :aria-label="chartAriaLabel"
        :height="320"
      />
    </div>
  </section>
</template>

<style scoped>
.history-page {
  position: relative;
  z-index: 0;
  display: grid;
  gap: 1.125rem;
  min-width: 0;
  padding: 0.25rem 0 1.75rem;
}

.history-page::before {
  position: absolute;
  z-index: -1;
  inset: -1.25rem -1.5rem -2rem;
  border: 1px solid color-mix(in srgb, var(--border) 72%, transparent);
  border-radius: 1.25rem;
  background: color-mix(in srgb, var(--app-bg) 88%, var(--panel-bg));
  box-shadow: 0 18px 48px var(--shadow-soft);
  content: '';
  backdrop-filter: blur(10px) saturate(115%);
}

.history-head {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 1rem;
  flex-wrap: wrap;
}

.history-head-copy {
  display: grid;
  gap: 0.35rem;
}

.history-kicker {
  display: inline-flex;
  align-items: center;
  gap: 0.45rem;
  color: var(--brand);
  font-size: 0.68rem;
  font-weight: 750;
  letter-spacing: 0.12em;
  line-height: 1;
  text-transform: uppercase;
}

.history-kicker-dot {
  width: 0.45rem;
  height: 0.45rem;
  border-radius: 999px;
  background: var(--accent-green);
  box-shadow: 0 0 0 0.25rem color-mix(in srgb, var(--accent-green) 18%, transparent);
}

.history-head h1 {
  margin: 0.1rem 0 0;
  font-size: 1.65rem;
  line-height: 1.12;
  letter-spacing: 0;
}

.muted {
  margin: 0;
  color: var(--muted);
  font-size: 0.875rem;
}

.refresh-button,
.link-button {
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
  border: 1px solid var(--border);
  background: var(--panel-bg);
  color: var(--text);
  border-radius: 0.55rem;
  padding: 0.55rem 0.85rem;
  cursor: pointer;
  font: inherit;
  box-shadow: 0 4px 14px var(--shadow-soft);
}

.refresh-button:hover:not(:disabled) {
  border-color: var(--border-strong);
  background: var(--button-hover);
  transform: translateY(-1px);
}

.refresh-button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.refresh-button svg.spinning {
  animation: history-spin 0.9s linear infinite;
}

.link-button {
  border: 0;
  background: transparent;
  color: var(--primary);
  padding: 0;
  text-decoration: underline;
  box-shadow: none;
}

.history-filter-band {
  display: grid;
  gap: 1rem;
  padding: 1rem;
  border: 1px solid var(--border);
  border-radius: 0.75rem;
  background: color-mix(in srgb, var(--panel-bg) 92%, transparent);
  box-shadow: 0 8px 24px var(--shadow-soft);
}

.history-filter-intro {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  min-width: 0;
}

.history-filter-intro > div {
  display: grid;
  gap: 0.22rem;
  min-width: 0;
}

.history-filter-label {
  color: var(--muted);
  font-size: 0.7rem;
  font-weight: 700;
  letter-spacing: 0.09em;
  text-transform: uppercase;
}

.history-filter-intro strong {
  overflow: hidden;
  color: var(--text);
  font-size: 0.95rem;
  font-weight: 700;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.history-filter-state {
  flex: 0 0 auto;
  border: 1px solid var(--border);
  border-radius: 999px;
  color: var(--muted);
  font-size: 0.72rem;
  padding: 0.3rem 0.58rem;
}

.history-filter-state.active {
  border-color: color-mix(in srgb, var(--accent-green) 44%, var(--border));
  background: color-mix(in srgb, var(--accent-green) 12%, transparent);
  color: var(--success-text);
}

.history-filters {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 0.75rem;
}

.filter-field {
  display: grid;
  gap: 0.4rem;
  min-width: 0;
}

.filter-field span {
  color: var(--muted);
  font-size: 0.72rem;
  font-weight: 650;
}

.filter-field select {
  width: 100%;
  height: 2.65rem;
  border: 1px solid var(--border);
  border-radius: 0.5rem;
  background: var(--subtle-bg);
  color: var(--text);
  padding: 0 0.7rem;
  font: inherit;
  transition: border-color 0.18s ease, background 0.18s ease, box-shadow 0.18s ease;
}

.filter-field select:hover:not(:disabled) {
  border-color: var(--border-strong);
  background: var(--button-hover);
}

.filter-field select:disabled {
  cursor: not-allowed;
  opacity: 0.65;
}

.notice {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  padding: 0.75rem 1rem;
  border: 1px solid var(--border);
  border-radius: 0.65rem;
  background: var(--panel-bg);
}

.notice.error {
  border-color: color-mix(in srgb, var(--danger) 45%, var(--border));
  background: color-mix(in srgb, var(--danger) 10%, var(--panel-bg));
  color: var(--danger);
}

.notice.capability {
  border-color: color-mix(in srgb, var(--accent-warn) 42%, var(--border));
  background: color-mix(in srgb, var(--accent-warn) 10%, var(--panel-bg));
  color: var(--warn-text);
}

.chart-panel {
  min-width: 0;
}

@keyframes history-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (max-width: 900px) {
  .history-filters {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 560px) {
  .history-page::before {
    inset: -0.75rem -0.7rem -1.25rem;
    border-radius: 0.9rem;
  }

  .history-filter-intro {
    align-items: flex-start;
    flex-direction: column;
    gap: 0.6rem;
  }

  .history-filters {
    grid-template-columns: 1fr;
  }
}
</style>
