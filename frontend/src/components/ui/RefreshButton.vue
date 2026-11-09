<script setup lang="ts">
import { RefreshCw } from '@lucide/vue'
import { computed, ref } from 'vue'

const props = withDefaults(defineProps<{
  label: string
  loadingLabel: string
  action: () => unknown | Promise<unknown>
  loading?: boolean
  disabled?: boolean
}>(), {
  loading: false,
  disabled: false,
})

const pending = ref(false)
const busy = computed(() => props.loading || pending.value)

async function refresh() {
  if (busy.value || props.disabled) return
  pending.value = true
  try {
    await props.action()
  } finally {
    pending.value = false
  }
}
</script>

<template>
  <button
    type="button"
    class="secondary refresh-button"
    :disabled="disabled || busy"
    :aria-busy="busy"
    :aria-label="busy ? loadingLabel : label"
    :title="label"
    @click="refresh"
  >
    <RefreshCw :size="15" class="refresh-button-icon" :class="{ 'is-spinning': busy }" aria-hidden="true" />
    <span>{{ busy ? loadingLabel : label }}</span>
  </button>
</template>

<style scoped>
.refresh-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  gap: 8px;
  min-width: 92px;
  height: 36px;
  min-height: 36px;
  padding: 0 12px;
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  background: var(--button-bg);
  color: var(--button-text);
  font-size: 12px;
  font-weight: 600;
  line-height: 1;
  white-space: nowrap;
}
.refresh-button:hover:not(:disabled) {
  border-color: var(--border-strong);
  background: var(--button-hover);
}
.refresh-button:disabled {
  cursor: not-allowed;
  opacity: 0.6;
}
.refresh-button-icon { flex-shrink: 0; }
.refresh-button-icon.is-spinning { animation: refresh-button-spin 0.9s linear infinite; }
@keyframes refresh-button-spin { to { transform: rotate(360deg); } }
@media (prefers-reduced-motion: reduce) {
  .refresh-button, .refresh-button-icon { animation: none !important; transition: none !important; }
}
</style>
