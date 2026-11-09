<script setup lang="ts">
interface MetricItem {
  label: string
  value: string
  detail?: string
  tone?: 'default' | 'success' | 'warning' | 'danger'
}

const props = withDefaults(
  defineProps<{
    items: MetricItem[]
    ariaLabel?: string
    emptyLabel?: string
  }>(),
  {
    ariaLabel: '',
    emptyLabel: '',
  },
)

function getToneClass(tone?: string) {
  if (!tone || tone === 'default') return 'metric-default'
  if (tone === 'success' || tone === 'warning' || tone === 'danger') {
    return `metric-${tone}`
  }
  return 'metric-default'
}

function itemKey(item: MetricItem, index: number) {
  return `${item.label || 'metric'}-${index}`
}
</script>

<template>
  <div
    class="p2-metric-strip"
    role="list"
    :aria-label="props.ariaLabel"
    :aria-busy="false"
  >
    <template v-if="props.items.length > 0">
      <div
        v-for="(item, index) in props.items"
        :key="itemKey(item, index)"
        class="metric-item"
        :class="getToneClass(item.tone)"
        role="listitem"
      >
        <div class="metric-label">{{ item.label }}</div>
        <div class="metric-value">
          {{ item.value }}
        </div>
        <div v-if="item.detail" class="metric-detail">
          {{ item.detail }}
        </div>
      </div>
    </template>
    <div v-else class="metric-empty" role="status">
      {{ props.emptyLabel }}
    </div>
  </div>
</template>

<style scoped>
.p2-metric-strip {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
  gap: 0.75rem;
  min-height: 0;
  padding: 0;
  background: transparent;
  font-size: var(--font-size-body);
}

.metric-item {
  position: relative;
  min-width: 0;
  max-width: none;
  overflow: hidden;
  padding: 0.9rem 1rem 0.85rem;
  border: 1px solid var(--border);
  border-radius: 0.7rem;
  background: var(--panel-bg);
  text-align: left;
  box-shadow: 0 8px 24px var(--shadow-soft);
}

.metric-item::before {
  position: absolute;
  inset: 0 auto 0 0;
  width: 3px;
  background: var(--primary);
  content: '';
}

.metric-item.metric-success::before {
  background: var(--success-text);
}

.metric-item.metric-warning::before {
  background: var(--accent-warn);
}

.metric-item.metric-danger::before {
  background: var(--danger-text);
}

.metric-label {
  overflow: hidden;
  margin-bottom: 0.45rem;
  color: var(--muted);
  font-size: var(--font-size-label);
  font-weight: 700;
  letter-spacing: 0.04em;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.metric-value {
  overflow-wrap: anywhere;
  color: var(--text);
  font-size: 1.45rem;
  font-variant-numeric: tabular-nums;
  font-weight: 700;
  line-height: 1.1;
}

.metric-success .metric-value {
  color: var(--success-text);
}

.metric-warning .metric-value {
  color: var(--warn-text);
}

.metric-danger .metric-value {
  color: var(--danger-text);
}

.metric-detail {
  min-height: 1.25rem;
  margin-top: 0.5rem;
  color: var(--muted);
  font-size: var(--font-size-help);
  line-height: 1.6;
}

.metric-empty {
  width: 100%;
  padding: 0.75rem;
  color: var(--muted);
  font-size: var(--font-size-body);
  text-align: center;
}

@media (max-width: 560px) {
  .p2-metric-strip {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .metric-item {
    padding-inline: 0.85rem;
  }

  .metric-value {
    font-size: 1.2rem;
  }
}
</style>
