<script setup lang="ts">
import { X } from '@lucide/vue'
import { useSlots } from 'vue'
import {
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogOverlay,
  DialogPortal,
  DialogRoot,
  DialogTitle,
} from 'reka-ui'

withDefaults(defineProps<{
  open: boolean
  title: string
  description: string
  closeLabel: string
  contentClass?: string
  showDescription?: boolean
}>(), {
  contentClass: '',
  showDescription: false,
})

const emit = defineEmits<{
  'update:open': [value: boolean]
}>()

const hasFooter = Boolean(useSlots().footer)
</script>

<template>
  <DialogRoot :open="open" @update:open="emit('update:open', $event)">
    <DialogPortal>
      <DialogOverlay class="settings-dialog-overlay" />
      <DialogContent
        :class="['settings-dialog-content', contentClass, { 'has-footer': hasFooter }]"
        aria-modal="true"
      >
        <header class="settings-dialog-head">
          <span class="settings-dialog-icon" aria-hidden="true">
            <slot name="icon" />
          </span>
          <div class="settings-dialog-heading">
            <DialogTitle class="settings-dialog-title">{{ title }}</DialogTitle>
            <DialogDescription
              :class="['settings-dialog-description', { 'is-visible': showDescription }]"
            >
              {{ description }}
            </DialogDescription>
          </div>
          <DialogClose as-child>
            <button
              class="icon-button settings-dialog-close"
              type="button"
              :aria-label="closeLabel"
            >
              <X :size="17" aria-hidden="true" />
            </button>
          </DialogClose>
        </header>
        <div v-if="hasFooter" class="settings-dialog-body">
          <slot />
        </div>
        <slot v-else />
        <footer v-if="hasFooter" class="settings-dialog-footer">
          <slot name="footer" />
        </footer>
      </DialogContent>
    </DialogPortal>
  </DialogRoot>
</template>

<style scoped>
.settings-dialog-description.is-visible {
  position: static;
  width: auto;
  height: auto;
  margin: 3px 0 0;
  overflow: visible;
  clip: auto;
  clip-path: none;
  color: var(--muted);
  font-size: 12px;
  line-height: 1.45;
  white-space: normal;
}
</style>
