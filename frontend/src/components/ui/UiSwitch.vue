<script setup lang="ts">
withDefaults(defineProps<{
  modelValue: boolean
  label: string
  title?: string
  disabled?: boolean
  size?: 'default' | 'compact'
  tone?: 'success' | 'danger'
}>(), {
  title: '',
  disabled: false,
  size: 'default',
  tone: 'success',
})

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
}>()
</script>

<template>
  <button
    :class="[
      'settings-card-switch',
      {
        'is-on': modelValue,
        'is-compact': size === 'compact',
        'is-danger': tone === 'danger',
      },
    ]"
    type="button"
    role="switch"
    :aria-checked="modelValue"
    :aria-label="label"
    :aria-disabled="disabled"
    :title="title || label"
    :disabled="disabled"
    @click="emit('update:modelValue', !modelValue)"
  >
    <span class="settings-card-switch-track" aria-hidden="true">
      <span class="settings-card-switch-knob" />
    </span>
  </button>
</template>
