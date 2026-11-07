<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'

interface Series {
  name: string
  values: number[]
  color?: string
}

type ValueFormatter = (value: number) => string

const props = withDefaults(
  defineProps<{
    title?: string
    labels: string[]
    series: Series[]
    height?: number
    emptyLabel?: string
    ariaLabel?: string
    labelPrefix?: string
    valuePrefix?: string
    valueSuffix?: string
    valueFormatter?: ValueFormatter
  }>(),
  {
    title: '',
    height: 200,
    emptyLabel: '',
    ariaLabel: '',
    labelPrefix: '',
    valuePrefix: '',
    valueSuffix: '',
    valueFormatter: undefined,
  },
)

const height = computed(() => Math.max(180, props.height || 200))

const normalizedSeries = computed(() => {
  const labelCount = Array.isArray(props.labels) ? props.labels.length : 0
  if (!Array.isArray(props.series) || labelCount === 0) return [] as Series[]
  return props.series
    .filter((item) => item && typeof item.name === 'string')
    .map((item) => {
      const rawValues = Array.isArray(item.values) ? item.values : []
      const length = Math.min(labelCount, rawValues.length)
      const values: number[] = []
      for (let i = 0; i < length; i += 1) {
        const value = rawValues[i]
        values.push(Number.isFinite(value) && value >= 0 ? value : Number.NaN)
      }
      while (values.length < labelCount) values.push(Number.NaN)
      return {
        name: item.name,
        color: item.color,
        values,
      }
    })
    .filter((item) => item.values.some((value) => Number.isFinite(value) && value >= 0))
})

const maxValue = computed(() => {
  let max = 0
  for (const series of normalizedSeries.value) {
    for (const value of series.values) {
      if (Number.isFinite(value) && value > max) max = value
    }
  }
  return max
})

function niceScale(value: number) {
  if (!Number.isFinite(value) || value <= 0) return 1
  const magnitude = 10 ** Math.floor(Math.log10(value))
  const normalized = value / magnitude
  const step = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10
  return step * magnitude
}

const chartScale = computed(() => niceScale(maxValue.value))
const axisTicks = computed(() => Array.from({ length: 5 }, (_, index) => chartScale.value * (1 - index / 4)))
const hasData = computed(() => normalizedSeries.value.length > 0)
const showPoints = computed(() => props.labels.length <= 31)
const xAxisLabelText = computed(() => props.labels.map(formatDateLabel).join('\n'))
const xAxisLayout = ref({ plotWidth: 0, labelWidth: 0 })
const xAxisTicks = computed(() => {
  const count = props.labels.length
  if (count === 0) return [] as number[]
  if (count === 1) return [0]
  const { plotWidth, labelWidth } = xAxisLayout.value
  // Until DOM measurements are available, keep only the two endpoints (also for SSR).
  if (plotWidth <= 0 || labelWidth <= 0) return [0, count - 1]
  for (let tickCount = Math.min(7, count); tickCount >= 2; tickCount -= 1) {
    const ticks = Array.from({ length: tickCount }, (_, index) => Math.round((index * (count - 1)) / (tickCount - 1)))
    let previousRight = -Infinity
    const fits = ticks.every((index) => {
      const x = safeX(index, count) / 100 * plotWidth
      const left = index === 0 ? 0 : index === count - 1 ? plotWidth - labelWidth : x - labelWidth / 2
      const separated = left - previousRight >= 4
      previousRight = left + labelWidth
      return separated
    })
    if (fits) return ticks
  }
  return [0, count - 1]
})

const chartElement = ref<HTMLElement | null>(null)
const plotElement = ref<HTMLElement | null>(null)
const tooltipElement = ref<HTMLElement | null>(null)
const xAxisLabelElement = ref<HTMLElement | null>(null)
const tooltipPosition = ref({ left: 8, top: 8 })
let resizeObserver: ResizeObserver | undefined
let fallbackWindow: Window | undefined

const tooltip = ref<{ visible: boolean; x: number; y: number; index: number }>({
  visible: false,
  x: 0,
  y: 0,
  index: -1,
})

watch(
  () => [props.labels, props.series],
  () => {
    tooltip.value.visible = false
    tooltip.value.index = -1
  },
  { deep: true },
)

watch([tooltip, tooltipElement], updateTooltipPosition, { flush: 'post' })
watch([chartElement, plotElement, tooltipElement, xAxisLabelElement], observeChartSize, { flush: 'post' })

onMounted(() => {
  if (typeof ResizeObserver !== 'undefined') {
    resizeObserver = new ResizeObserver(updateChartSize)
  } else if (typeof window !== 'undefined' && typeof window.addEventListener === 'function') {
    window.addEventListener('resize', updateChartSize)
    fallbackWindow = window
  }
  observeChartSize()
})

onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  if (typeof fallbackWindow?.removeEventListener === 'function') {
    fallbackWindow.removeEventListener('resize', updateChartSize)
  }
  fallbackWindow = undefined
})


function observeChartSize() {
  resizeObserver?.disconnect()
  for (const element of [chartElement.value, plotElement.value, tooltipElement.value, xAxisLabelElement.value]) {
    if (element) resizeObserver?.observe(element)
  }
  updateChartSize()
}

function updateChartSize() {
  const plotWidth = plotElement.value?.getBoundingClientRect().width || 0
  const labelWidth = xAxisLabelElement.value?.getBoundingClientRect().width || 0
  if (plotWidth !== xAxisLayout.value.plotWidth || labelWidth !== xAxisLayout.value.labelWidth) {
    xAxisLayout.value = { plotWidth, labelWidth }
  }
  updateTooltipPosition()
}

function updateTooltipPosition() {
  const chart = chartElement.value
  const plot = plotElement.value
  const tip = tooltipElement.value
  if (!tooltip.value.visible || !chart || !plot || !tip) return
  const chartRect = chart.getBoundingClientRect()
  const plotRect = plot.getBoundingClientRect()
  const tipRect = tip.getBoundingClientRect()
  const inset = 8
  const gap = 10
  const x = plotRect.left - chartRect.left - chart.clientLeft + plotRect.width * tooltip.value.x / 100
  const y = plotRect.top - chartRect.top - chart.clientTop + plotRect.height * tooltip.value.y / 100
  const clamp = (value: number, maximum: number) => Math.max(inset, Math.min(value, maximum - inset))
  const above = y - gap - tipRect.height
  tooltipPosition.value = {
    left: clamp(x - tipRect.width / 2, chart.clientWidth - tipRect.width),
    top: clamp(above >= inset ? above : y + gap, chart.clientHeight - tipRect.height),
  }
}

function pointsFor(values: number[]) {
  return values
    .map((val, idx) => ({ val, idx }))
    .filter(({ val }) => Number.isFinite(val) && val >= 0)
}

function markersFor(values: number[]) {
  return pointsFor(values).filter(({ idx }) => showPoints.value || (
    !Number.isFinite(values[idx - 1]) && !Number.isFinite(values[idx + 1])
  ))
}

function getPath(values: number[], scale: number) {
  if (values.length === 0) return ''
  let path = ''
  let hasPrevious = false
  for (let i = 0; i < values.length; i += 1) {
    const val = values[i]
    if (!Number.isFinite(val) || val < 0) {
      hasPrevious = false
      continue
    }
    const x = safeX(i, values.length)
    const y = safeY(val, scale)
    path += hasPrevious ? ` L ${x},${y}` : `M ${x},${y}`
    hasPrevious = true
  }
  return path
}

function getAreaPath(values: number[], scale: number) {
  const points = pointsFor(values)
  if (points.length < 2 || points.length !== values.length) return ''
  const first = points[0]
  const last = points[points.length - 1]
  let path = `M ${safeX(first.idx, values.length)},100`
  for (const point of points) {
    path += ` L ${safeX(point.idx, values.length)},${safeY(point.val, scale)}`
  }
  path += ` L ${safeX(last.idx, values.length)},100 Z`
  return path
}

function safeX(index: number, count: number) {
  if (count <= 1) return 50
  const pos = (index / (count - 1)) * 100
  return Math.round(Math.min(Math.max(pos, 0), 100) * 100) / 100
}

function safeY(value: number, scale: number) {
  if (!Number.isFinite(value) || scale <= 0) return 100
  const y = 100 - (value / scale) * 100
  return Math.round(Math.max(0, Math.min(100, y)) * 100) / 100
}

function handleMouseMove(event: MouseEvent) {
  if (!hasData.value || props.labels.length === 0) {
    tooltip.value.visible = false
    return
  }
  const plot = event.currentTarget as HTMLElement
  const rect = plot.getBoundingClientRect()
  if (rect.width <= 0 || rect.height <= 0) {
    tooltip.value.visible = false
    return
  }
  const x = Math.max(0, Math.min(rect.width, event.clientX - rect.left))
  const y = Math.max(0, Math.min(rect.height, event.clientY - rect.top))
  const maxIndex = props.labels.length - 1
  const index = Math.max(0, Math.min(Math.round((x / rect.width) * maxIndex), maxIndex))
  // Keep the anchor still at the selected date so the pointer can enter the tooltip.
  if (tooltip.value.visible && tooltip.value.index === index) return
  tooltip.value = {
    visible: true,
    x: safeX(index, props.labels.length),
    y: (y / rect.height) * 100,
    index,
  }
}

function handleMouseLeave() {
  tooltip.value.visible = false
}

function seriesColor(series: Series, index: number) {
  return series.color || colors[index % colors.length]
}

function formatCompact(value: number) {
  if (!Number.isFinite(value)) return '—'
  const absolute = Math.abs(value)
  if (absolute >= 1_000_000_000) return `${(value / 1_000_000_000).toFixed(1)}B`
  if (absolute >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`
  if (absolute >= 1_000) return `${(value / 1_000).toFixed(1)}K`
  return value.toLocaleString(undefined, { maximumFractionDigits: 1 })
}

function formatValue(value: number) {
  const formatted = props.valueFormatter?.(value) || formatCompact(value)
  return `${props.valuePrefix || ''}${formatted}${props.valueSuffix || ''}`
}

function formatAxisValue(value: number) {
  const formatted = props.valueFormatter?.(value) || formatCompact(value)
  return `${formatted}${props.valueSuffix || ''}`
}

function formatDateLabel(label: string) {
  const match = /^(?:\d{4}-)?(\d{2})-(\d{2})$/.exec(label)
  return match ? `${match[1]}/${match[2]}` : label
}

function tooltipValue(series: Series, index: number) {
  const value = series.values[index]
  return Number.isFinite(value) ? formatValue(value) : '—'
}

function xLabelStyle(index: number) {
  const lastIndex = props.labels.length - 1
  const x = safeX(index, props.labels.length)
  if (index === 0) return { left: `${x}%`, transform: 'translateX(0)' }
  if (index === lastIndex) return { left: `${x}%`, transform: 'translateX(-100%)' }
  return { left: `${x}%`, transform: 'translateX(-50%)' }
}

const colors = ['var(--brand)', 'var(--accent-green)', 'var(--accent-warn)', 'var(--danger-text)']
</script>

<template>
  <div ref="chartElement" class="p2-line-chart" :style="{ height: `${height}px` }" @mouseleave="handleMouseLeave" @keydown.esc="handleMouseLeave">
    <div v-if="title || normalizedSeries.length" class="chart-header">
      <div v-if="title" class="chart-title">{{ title }}</div>
      <div v-if="normalizedSeries.length" class="chart-legend" aria-hidden="true">
        <span v-for="(s, idx) in normalizedSeries" :key="`legend-${s.name || idx}`" class="legend-item">
          <span class="legend-dot" :style="{ backgroundColor: seriesColor(s, idx) }" />
          {{ s.name }}
        </span>
      </div>
    </div>

    <div class="chart-body">
      <div v-if="!hasData" class="empty-state" role="status">
        {{ emptyLabel }}
      </div>

      <template v-else>
        <div class="axis-y" aria-hidden="true">
          <span
            v-for="(tick, index) in axisTicks"
            :key="`axis-y-${index}`"
            :style="{ top: `${index * 25}%` }"
          >
            {{ formatAxisValue(tick) }}
          </span>
        </div>

        <div ref="plotElement" class="chart-plot" @mousemove="handleMouseMove">
          <svg
            viewBox="0 0 100 100"
            preserveAspectRatio="none"
            class="line-chart-svg"
            role="img"
            :aria-label="ariaLabel || title || emptyLabel"
          >
            <g class="grid-line">
              <line v-for="(_, index) in axisTicks" :key="`grid-${index}`" x1="0" :y1="index * 25" x2="100" :y2="index * 25" />
            </g>

            <line
              v-if="tooltip.visible"
              class="crosshair"
              :x1="tooltip.x"
              y1="0"
              :x2="tooltip.x"
              y2="100"
            />

            <path
              v-if="normalizedSeries.length === 1"
              class="series-area"
              :d="getAreaPath(normalizedSeries[0].values, chartScale)"
              :fill="seriesColor(normalizedSeries[0], 0)"
            />

            <g v-for="(s, sIdx) in normalizedSeries" :key="s.name || `series-${sIdx}`">
              <path
                class="series-path"
                :d="getPath(s.values, chartScale)"
                :stroke="seriesColor(s, sIdx)"
              />
            </g>
          </svg>

          <div class="chart-markers" aria-hidden="true">
            <template v-for="(s, sIdx) in normalizedSeries" :key="`markers-${s.name || sIdx}`">
              <span
                v-for="point in markersFor(s.values)"
                :key="`${s.name}-${point.idx}`"
                class="data-point"
                :style="{
                  left: `${safeX(point.idx, s.values.length)}%`,
                  top: `${safeY(point.val, chartScale)}%`,
                  backgroundColor: seriesColor(s, sIdx),
                }"
              />
            </template>
          </div>

          <template v-if="tooltip.visible && tooltip.index >= 0">
            <span
              v-for="(s, sIdx) in normalizedSeries"
              :key="`focus-${s.name || sIdx}`"
              v-show="Number.isFinite(s.values[tooltip.index])"
              class="focus-marker"
              :style="{
                left: `${tooltip.x}%`,
                top: `${safeY(s.values[tooltip.index], chartScale)}%`,
                backgroundColor: seriesColor(s, sIdx),
              }"
            />
          </template>
        </div>

        <div class="axis-x" aria-hidden="true">
          <div ref="xAxisLabelElement" class="axis-x-measure">{{ xAxisLabelText }}</div>
          <span
            v-for="index in xAxisTicks"
            :key="`axis-x-${index}`"
            :style="xLabelStyle(index)"
          >
            {{ formatDateLabel(labels[index] || '—') }}
          </span>
        </div>
      </template>
    </div>

    <div
      v-if="tooltip.visible && tooltip.index >= 0"
      ref="tooltipElement"
      class="chart-tooltip"
      role="tooltip"
      tabindex="0"
      :style="{ left: `${tooltipPosition.left}px`, top: `${tooltipPosition.top}px` }"
    >
      <div class="tooltip-date">{{ `${labelPrefix}${formatDateLabel(labels[tooltip.index] || '—')}` }}</div>
      <div class="tooltip-list">
        <div v-for="(s, idx) in normalizedSeries" :key="`tip-${s.name || idx}`" class="tooltip-series">
          <span class="tooltip-series-name">
            <span class="legend-dot" :style="{ backgroundColor: seriesColor(s, idx) }" />
            {{ s.name }}
          </span>
          <strong>{{ tooltipValue(s, tooltip.index) }}</strong>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.p2-line-chart {
  position: relative;
  display: flex;
  flex-direction: column;
  width: 100%;
  min-height: 180px;
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: 0.75rem;
  background: var(--panel-bg);
  box-shadow: 0 10px 28px var(--shadow-soft);
}

.chart-header {
  display: flex;
  flex: 0 0 auto;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  min-height: 3.35rem;
  padding: 0.85rem 1.1rem 0.7rem;
  border-bottom: 1px solid var(--border-soft);
}

.chart-title {
  flex: 0 1 auto;
  min-width: 0;
  overflow: hidden;
  color: var(--text);
  font-size: 0.92rem;
  font-weight: 750;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.chart-legend {
  display: flex;
  flex: 0 1 auto;
  align-items: center;
  gap: 0.8rem;
  min-width: 0;
  margin-left: auto;
  overflow-x: auto;
  color: var(--muted);
  font-size: 0.7rem;
}

.legend-item,
.tooltip-series-name {
  display: inline-flex;
  align-items: center;
  gap: 0.35rem;
  min-width: 0;
}

.legend-item {
  flex: 0 0 auto;
  white-space: nowrap;
}

.legend-dot {
  display: inline-block;
  flex: 0 0 auto;
  width: 0.45rem;
  height: 0.45rem;
  border-radius: 999px;
}

.chart-body {
  position: relative;
  display: grid;
  flex: 1 1 0;
  grid-template-columns: max-content minmax(0, 1fr);
  grid-template-rows: minmax(0, 1fr) 1.05rem;
  gap: 0.7rem 0.55rem;
  min-height: 0;
  padding: 0.75rem 1.1rem 0.35rem 0.55rem;
}

.axis-y {
  display: grid;
  grid-area: 1 / 1;
  grid-template-rows: minmax(0, 1fr);
  color: var(--muted);
  font-size: 0.66rem;
  font-variant-numeric: tabular-nums;
  pointer-events: none;
}

.axis-y span {
  position: relative;
  grid-area: 1 / 1;
  align-self: start;
  justify-self: end;
  transform: translateY(-50%);
  white-space: nowrap;
}

.chart-plot {
  position: relative;
  grid-area: 1 / 2;
  min-width: 0;
  min-height: 0;
  cursor: crosshair;
}

.line-chart-svg {
  display: block;
  width: 100%;
  height: 100%;
  overflow: visible;
}

.grid-line line {
  stroke: var(--border);
  stroke-dasharray: 3 5;
  opacity: 0.72;
  vector-effect: non-scaling-stroke;
}

.crosshair {
  stroke: var(--brand);
  stroke-dasharray: 3 4;
  stroke-width: 1;
  opacity: 0.68;
  vector-effect: non-scaling-stroke;
}

.series-area {
  opacity: 0.1;
}

.series-path {
  fill: none;
  stroke-linecap: round;
  stroke-linejoin: round;
  stroke-width: 2.4;
  vector-effect: non-scaling-stroke;
}


.chart-markers {
  position: absolute;
  inset: 0;
  pointer-events: none;
}

.data-point,
.focus-marker {
  position: absolute;
  border: 2px solid var(--panel-bg);
  border-radius: 50%;
  box-shadow: 0 1px 5px var(--shadow-soft);
  transform: translate(-50%, -50%);
}

.data-point {
  width: 0.48rem;
  height: 0.48rem;
}

.focus-marker {
  z-index: 1;
  width: 0.8rem;
  height: 0.8rem;
  border-width: 2px;
  outline: 1px solid var(--panel-bg);
}


.chart-tooltip {
  position: absolute;
  z-index: 2;
  display: flex;
  flex-direction: column;
  width: max-content;
  min-width: min(9.5rem, calc(100% - 1rem));
  max-width: calc(100% - 1rem);
  max-height: calc(100% - 1rem);
  padding: 0.6rem 0.7rem;
  border: 1px solid var(--border-strong);
  border-radius: 0.55rem;
  background: color-mix(in srgb, var(--panel-bg) 94%, transparent);
  box-shadow: 0 10px 24px var(--shadow);
  color: var(--text);
  backdrop-filter: blur(8px) saturate(120%);
}

.tooltip-list {
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
}

.tooltip-date {
  flex: 0 0 auto;
  margin-bottom: 0.4rem;
  color: var(--muted);
  font-size: 0.68rem;
  font-weight: 700;
  letter-spacing: 0.04em;
}

.tooltip-series {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  font-size: 0.72rem;
  line-height: 1.5;
}

.tooltip-series-name {
  color: var(--muted);
  overflow-wrap: anywhere;
}

.tooltip-series strong {
  flex: 0 0 auto;
  max-width: 65%;
  overflow-wrap: anywhere;
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.axis-x {
  position: relative;
  grid-area: 2 / 2;
  min-width: 0;
  color: var(--muted);
  font-size: 0.66rem;
  font-variant-numeric: tabular-nums;
  pointer-events: none;
}

.axis-x span {
  position: absolute;
  white-space: nowrap;
}

.axis-x .axis-x-measure {
  position: absolute;
  width: max-content;
  height: 0;
  overflow: hidden;
  visibility: hidden;
  white-space: pre;
}

.empty-state {
  position: absolute;
  inset: 0;
  display: grid;
  place-items: center;
  color: var(--muted);
  font-size: 0.875rem;
}

@media (max-width: 640px) {
  .chart-header {
    align-items: flex-start;
    flex-direction: column;
    gap: 0.55rem;
  }

  .chart-legend {
    width: 100%;
    margin-left: 0;
  }
}
</style>
