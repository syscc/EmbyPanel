<script setup lang="ts">
defineProps<{
  modelValue: boolean
  label: string
  disabled?: boolean
}>()

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
}>()

function handleChange(event: Event) {
  emit('update:modelValue', (event.currentTarget as HTMLInputElement).checked)
}
</script>

<template>
  <label class="check">
    <input
      type="checkbox"
      :checked="modelValue"
      :disabled="disabled"
      @change="handleChange"
    />
    <span>{{ label }}</span>
  </label>
</template>
