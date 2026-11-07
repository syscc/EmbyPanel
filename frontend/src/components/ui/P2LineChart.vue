<script setup lang="ts">
import { computed, ref, watch } from 'vue'

interface Series {
  name: string
  values: number[]
  color?: string
}

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
  }>(),
  {
    title: '',
    height: 200,
    emptyLabel: '',
    ariaLabel: '',
    labelPrefix: '',
    valuePrefix: '',
  },
)

const height = computed(() => Math.max(80, props.height || 200))

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
      // Pad trailing gaps so index alignment with labels stays explicit as NaN.
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

const chartScale = computed(() => (maxValue.value > 0 ? maxValue.value : 1))

const hasData = computed(() => normalizedSeries.value.length > 0)

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

function pointsFor(values: number[]) {
  return values
    .map((val, idx) => ({ val, idx }))
    .filter(({ val }) => Number.isFinite(val) && val >= 0)
}

function getPath(values: number[], scale: number, chartHeight: number) {
  if (values.length === 0) return ''
  let path = ''
  let hasPrevious = false
  const safeScale = scale > 0 ? scale : 1
  for (let i = 0; i < values.length; i += 1) {
    const val = values[i]
    if (!Number.isFinite(val) || val < 0) {
      hasPrevious = false
      continue
    }
    const x = safeX(i, values.length)
    const y = safeY(val, safeScale, chartHeight)
    path += hasPrevious ? ` L ${x},${y}` : `M ${x},${y}`
    hasPrevious = true
  }
  return path
}

function safeX(index: number, count: number) {
  if (count <= 0) return 0
  if (count <= 1) return 50
  const pos = (index / (count - 1)) * 100
  return Math.round(Math.min(Math.max(pos, 0), 100))
}

function safeY(value: number, scale: number, chartHeight: number) {
  if (!Number.isFinite(value) || scale <= 0) return chartHeight
  const y = chartHeight - (value / scale) * chartHeight
  return Math.round(Math.max(0, Math.min(chartHeight, y)))
}

function handleMouseMove(event: MouseEvent) {
  if (!hasData.value || props.labels.length === 0) {
    tooltip.value.visible = false
    return
  }
  const svg = event.currentTarget as SVGSVGElement
  const rect = svg.getBoundingClientRect()
  if (rect.width <= 0 || rect.height <= 0) {
    tooltip.value.visible = false
    return
  }
  const x = event.clientX - rect.left
  const y = event.clientY - rect.top
  const maxIndex = props.labels.length - 1
  const index = Math.max(
    0,
    Math.min(Math.round((x / rect.width) * maxIndex), maxIndex),
  )
  tooltip.value = {
    visible: true,
    x: Math.round((x / rect.width) * 100),
    y: Math.round((y / rect.height) * height.value * 0.35),
    index,
  }
}

function handleMouseLeave() {
  tooltip.value.visible = false
}

function seriesColor(series: Series, index: number) {
  return series.color || colors[index % colors.length]
}

function tooltipValue(series: Series, index: number) {
  const value = series.values[index]
  if (!Number.isFinite(value)) return '—'
  return `${props.valuePrefix || ''}${value}`
}

const colors = ['var(--primary)', 'var(--accent)', 'var(--success)', 'var(--warning)']
</script>

<template>
  <div class="p2-line-chart" :style="{ height: `${height}px` }">
    <div v-if="title" class="chart-title">{{ title }}</div>
    <svg
      :viewBox="`0 0 100 ${height}`"
      preserveAspectRatio="none"
      class="line-chart-svg"
      role="img"
      :aria-label="ariaLabel || title || emptyLabel"
      @mousemove="handleMouseMove"
      @mouseleave="handleMouseLeave"
    >
      <g v-if="!hasData" class="empty-state">
        <text
          x="50"
          :y="height / 2"
          text-anchor="middle"
          dominant-baseline="middle"
          fill="var(--muted-foreground)"
          font-size="4"
        >
          {{ emptyLabel }}
        </text>
      </g>

      <template v-else>
        <g class="grid-line">
          <line
            v-for="i in 5"
            :key="`grid-${i}`"
            x1="0"
            :y1="i * (height / 5)"
            x2="100"
            :y2="i * (height / 5)"
            stroke="var(--border)"
            stroke-width="0.5"
            stroke-dasharray="2 2"
          />
        </g>

        <g
          v-for="(s, sIdx) in normalizedSeries"
          :key="s.name || `series-${sIdx}`"
        >
          <path
            :d="getPath(s.values, chartScale, height)"
            fill="none"
            :stroke="seriesColor(s, sIdx)"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
          <circle
            v-for="point in pointsFor(s.values)"
            :key="`${s.name}-${point.idx}`"
            :cx="safeX(point.idx, s.values.length)"
            :cy="safeY(point.val, chartScale, height)"
            r="1.6"
            :fill="seriesColor(s, sIdx)"
          />
        </g>

        <g
          v-if="normalizedSeries.length > 0"
          class="legend"
          transform="translate(2, 4)"
        >
          <g
            v-for="(s, idx) in normalizedSeries"
            :key="`legend-${s.name || idx}`"
            :transform="`translate(0, ${idx * 6})`"
          >
            <rect
              x="0"
              y="0"
              width="4"
              height="4"
              :fill="seriesColor(s, idx)"
            />
            <text x="6" y="3.5" fill="var(--text)" font-size="3.2">
              {{ s.name }}
            </text>
          </g>
        </g>

        <g v-if="tooltip.visible && tooltip.index >= 0">
          <rect
            :x="Math.min(Math.max(tooltip.x - 18, 1), 64)"
            :y="Math.max(tooltip.y, 2)"
            :width="34"
            :height="6 + normalizedSeries.length * 5"
            rx="1"
            fill="var(--card-bg)"
            stroke="var(--border)"
            stroke-width="0.4"
          />
          <text
            :x="Math.min(Math.max(tooltip.x - 16, 3), 66)"
            :y="Math.max(tooltip.y, 2) + 4"
            fill="var(--text)"
            font-size="3"
          >
            <tspan
              :x="Math.min(Math.max(tooltip.x - 16, 3), 66)"
              dy="0"
            >
              {{ `${labelPrefix || ''}${labels[tooltip.index] ?? '—'}` }}
            </tspan>
            <tspan
              v-for="(s, i) in normalizedSeries"
              :key="`tip-${s.name || i}`"
              :x="Math.min(Math.max(tooltip.x - 16, 3), 66)"
              dy="4.5"
            >
              {{ `${s.name}: ${tooltipValue(s, tooltip.index)}` }}
            </tspan>
          </text>
        </g>
      </template>
    </svg>
  </div>
</template>

<style scoped>
.p2-line-chart {
  position: relative;
  width: 100%;
  background: var(--card-bg);
  border: 1px solid var(--border);
  border-radius: 0.5rem;
  overflow: hidden;
}

.chart-title {
  padding: 0.75rem 1rem 0;
  font-size: 0.875rem;
  color: var(--muted-foreground);
  font-weight: 500;
}

.line-chart-svg {
  width: 100%;
  height: calc(100% - 1.75rem);
  display: block;
}

.empty-state {
  fill: var(--muted-foreground);
}

.grid-line line {
  opacity: 0.5;
}
</style>
