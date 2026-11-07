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
        role="listitem"
      >
        <div class="metric-label">{{ item.label }}</div>
        <div class="metric-value" :class="getToneClass(item.tone)">
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
  display: flex;
  gap: 1.5rem;
  background: var(--card-bg);
  padding: 1rem;
  border-radius: 0.5rem;
  border: 1px solid var(--border);
  font-size: 0.875rem;
  flex-wrap: wrap;
  justify-content: center;
  min-height: 4.5rem;
}

.metric-item {
  flex: 1;
  min-width: 0;
  text-align: center;
  max-width: 200px;
}

.metric-label {
  color: var(--muted-foreground);
  font-size: 0.75rem;
  margin-bottom: 0.25rem;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.metric-value {
  font-size: 1.25rem;
  font-weight: 600;
  margin-bottom: 0.25rem;
}

.metric-default {
  color: var(--text);
}

.metric-success {
  color: var(--success, #16a34a);
}

.metric-warning {
  color: var(--warning, #d97706);
}

.metric-danger {
  color: var(--danger, #dc2626);
}

.metric-detail {
  font-size: 0.75rem;
  color: var(--muted-foreground);
  margin-top: 0.25rem;
}

.metric-empty {
  width: 100%;
  text-align: center;
  color: var(--muted-foreground);
  font-size: 0.875rem;
  padding: 0.5rem 0;
}
</style>
