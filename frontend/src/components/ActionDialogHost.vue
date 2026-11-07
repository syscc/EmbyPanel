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

let returnFocusTarget: HTMLElement | null = null
let dialogGeneration = 0
let scheduledRestoreGeneration: number | null = null

function isActionDialogElement(element: Element | null): boolean {
  return element instanceof HTMLElement && Boolean(element.closest('.action-dialog-content'))
}

function canRestoreFocus(element: HTMLElement): boolean {
  if (!element.isConnected || element.matches(':disabled')) return false
  if (element.getAttribute('aria-disabled') === 'true') return false
  if (element.closest('[hidden], [inert], [aria-hidden="true"]')) return false

  const style = window.getComputedStyle(element)
  return style.display !== 'none'
    && style.visibility !== 'hidden'
    && style.visibility !== 'collapse'
    && element.getClientRects().length > 0
}

function restoreFocus(generation: number) {
  if (generation !== dialogGeneration || open.value) return

  const target = returnFocusTarget
  const active = document.activeElement
  if (!target || !canRestoreFocus(target)) {
    returnFocusTarget = null
    return
  }
  if (active === target) {
    returnFocusTarget = null
    return
  }
  if (active instanceof Element
    && active.isConnected
    && active !== document.body
    && active !== document.documentElement) {
    returnFocusTarget = null
    return
  }

  target.focus({ preventScroll: true })
  returnFocusTarget = null
}

function scheduleFocusRestore(generation: number) {
  if (scheduledRestoreGeneration === generation) return
  scheduledRestoreGeneration = generation
  void nextTick(() => {
    window.requestAnimationFrame(() => {
      if (scheduledRestoreGeneration !== generation) return
      scheduledRestoreGeneration = null
      restoreFocus(generation)
    })
  })
}

function handleCloseAutoFocus(event: Event) {
  event.preventDefault()
  scheduleFocusRestore(dialogGeneration)
}

watch(open, (isOpen) => {
  if (!isOpen) {
    scheduleFocusRestore(dialogGeneration)
    return
  }

  dialogGeneration += 1
  const replacesClosingDialog = scheduledRestoreGeneration !== null
  scheduledRestoreGeneration = null
  const active = document.activeElement
  const preservePendingTarget = replacesClosingDialog
    && (!(active instanceof HTMLElement)
      || !active.isConnected
      || active === document.body
      || active === document.documentElement)
  if (!isActionDialogElement(active) && !preservePendingTarget) {
    returnFocusTarget = active instanceof HTMLElement
      && active !== document.body
      && active !== document.documentElement
      ? active
      : null
  }

  const generation = dialogGeneration
  void nextTick(() => {
    if (!open.value || generation !== dialogGeneration) return
    if (request.value?.kind === 'prompt') inputRef.value?.focus()
    else confirmRef.value?.focus()
  })
}, { flush: 'sync' })

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
        @close-auto-focus="handleCloseAutoFocus"
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
