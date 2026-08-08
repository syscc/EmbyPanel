<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { AlertTriangle, Check, KeyRound, X } from '@lucide/vue'
import {
  DialogContent,
  DialogDescription,
  DialogOverlay,
  DialogPortal,
  DialogRoot,
  DialogTitle,
} from 'reka-ui'
import { usePanelContext } from '@/composables/panel-context'
import { useActionDialog } from '@/composables/useActionDialog'

const { t } = usePanelContext()
const {
  open,
  request,
  inputValue,
  canConfirm,
  confirmActionDialog,
  cancelActionDialog,
  handleOpenChange,
} = useActionDialog()
const inputRef = ref<HTMLInputElement | null>(null)
const confirmRef = ref<HTMLButtonElement | null>(null)

watch(open, (isOpen) => {
  if (!isOpen) return
  void nextTick(() => {
    if (request.value?.kind === 'prompt') inputRef.value?.focus()
    else confirmRef.value?.focus()
  })
})

onBeforeUnmount(() => {
  if (open.value) cancelActionDialog()
})
</script>

<template>
  <DialogRoot :open="open" @update:open="handleOpenChange">
    <DialogPortal>
      <DialogOverlay class="settings-dialog-overlay action-dialog-overlay" />
      <DialogContent
        v-if="request"
        class="settings-dialog-content action-dialog-content"
        :class="`is-${request.tone || 'default'}`"
        aria-modal="true"
      >
        <header class="settings-dialog-head action-dialog-head">
          <span class="settings-dialog-icon action-dialog-icon" aria-hidden="true">
            <KeyRound v-if="request.kind === 'prompt'" :size="18" />
            <AlertTriangle v-else :size="18" />
          </span>
          <div class="settings-dialog-heading action-dialog-heading">
            <DialogTitle class="settings-dialog-title action-dialog-title">
              {{ request.title }}
            </DialogTitle>
            <DialogDescription
              v-if="request.description"
              class="action-dialog-description"
            >
              {{ request.description }}
            </DialogDescription>
          </div>
          <button
            v-if="request.tone !== 'danger'"
            class="icon-button settings-dialog-close action-dialog-close"
            type="button"
            :aria-label="t('关闭')"
            :title="t('关闭')"
            @click="cancelActionDialog"
          >
            <X :size="17" aria-hidden="true" />
          </button>
        </header>

        <form class="settings-dialog-form action-dialog-form" @submit.prevent="confirmActionDialog">
          <label v-if="request.kind === 'prompt'" class="action-dialog-input-label">
            <span>{{ request.inputLabel || t('输入') }}</span>
            <input
              ref="inputRef"
              v-model="inputValue"
              :type="request.inputType || 'text'"
              :placeholder="request.inputPlaceholder"
              :autocomplete="request.inputAutocomplete || (request.inputType === 'password' ? 'new-password' : 'off')"
              :minlength="request.minLength"
              :required="(request.minLength ?? 0) > 0"
            />
          </label>

          <div class="settings-dialog-actions action-dialog-actions">
            <button
              class="secondary"
              type="button"
              @click="cancelActionDialog"
            >
              {{ request.cancelText || t('取消') }}
            </button>
            <button
              ref="confirmRef"
              :class="request.tone === 'danger' ? 'danger-button' : 'primary'"
              type="submit"
              :disabled="!canConfirm"
            >
              <Check :size="15" aria-hidden="true" />
              {{ request.confirmText || t('确认') }}
            </button>
          </div>
        </form>
      </DialogContent>
    </DialogPortal>
  </DialogRoot>
</template>
