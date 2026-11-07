<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from 'vue'
import { RefreshCw } from '@lucide/vue'
import { useHistoryController } from '@/composables/useHistoryController'
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
      <div>
        <h1>{{ t('历史') }}</h1>
        <p class="muted">
          {{ t('按日聚合的反代请求统计趋势') }}
        </p>
      </div>
      <button
        type="button"
        class="refresh-button"
        :disabled="loading || refreshing"
        @click="refreshPage"
      >
        <RefreshCw :size="15" />
        {{ loading ? t('加载中') : t('刷新') }}
      </button>
    </header>

    <div class="history-filters" role="group" :aria-label="t('历史')">
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
        :empty-label="loading ? t('加载中') : t('无数据')"
        :aria-label="chartAriaLabel"
        :height="260"
      />
    </div>
  </section>
</template>

<style scoped>
.history-page {
  display: grid;
  gap: 1rem;
}

.history-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
  flex-wrap: wrap;
}

.history-head h1 {
  margin: 0;
  font-size: 1.35rem;
}

.muted {
  margin: 0.35rem 0 0;
  color: var(--muted-foreground);
  font-size: 0.875rem;
}

.refresh-button,
.link-button {
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
  border: 1px solid var(--border);
  background: var(--card-bg);
  color: var(--text);
  border-radius: 0.5rem;
  padding: 0.45rem 0.8rem;
  cursor: pointer;
  font: inherit;
}

.refresh-button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.link-button {
  border: none;
  background: transparent;
  color: var(--primary);
  padding: 0;
  text-decoration: underline;
}

.history-filters {
  display: flex;
  flex-wrap: wrap;
  gap: 0.75rem 1rem;
  padding: 0.85rem 1rem;
  border: 1px solid var(--border);
  border-radius: 0.5rem;
  background: var(--card-bg);
}

.filter-field {
  display: grid;
  gap: 0.35rem;
  min-width: 10rem;
  flex: 1;
}

.filter-field span {
  font-size: 0.75rem;
  color: var(--muted-foreground);
}

.filter-field select {
  border: 1px solid var(--border);
  border-radius: 0.4rem;
  background: var(--bg);
  color: var(--text);
  padding: 0.45rem 0.55rem;
  font: inherit;
}

.notice {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  padding: 0.75rem 1rem;
  border-radius: 0.5rem;
  border: 1px solid var(--border);
}

.notice.error {
  border-color: color-mix(in srgb, var(--danger) 45%, var(--border));
  background: color-mix(in srgb, var(--danger) 10%, var(--card-bg));
  color: var(--danger);
}

.chart-panel {
  min-height: 260px;
}
</style>
