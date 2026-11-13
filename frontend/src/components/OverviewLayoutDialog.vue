<script setup lang="ts">
import { ArrowDown, ArrowUp, GripVertical, SlidersHorizontal } from '@lucide/vue'
import { ref } from 'vue'
import SettingsDialogShell from '@/components/ui/SettingsDialogShell.vue'
import CheckboxField from '@/components/ui/CheckboxField.vue'
import { usePanelContext } from '@/composables/panel-context'
import type { OverviewModuleId, OverviewModuleView } from '@/composables/useOverviewLayout'

const props = defineProps<{
  open: boolean
  modules: OverviewModuleView[]
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
  'toggle': [id: OverviewModuleId]
  'move': [id: OverviewModuleId, direction: -1 | 1]
  'reorder': [sourceId: OverviewModuleId, targetId: OverviewModuleId]
  'save': []
  'reset': []
}>()

const { t } = usePanelContext()

// Drag state lives in the dialog: it only highlights the draft list and never
// touches the applied layout on the dashboard.
const draggedId = ref<OverviewModuleId | null>(null)
const dragOverId = ref<OverviewModuleId | null>(null)

function clearDragState() {
  draggedId.value = null
  dragOverId.value = null
}

function handleDragStart(id: OverviewModuleId, event: DragEvent) {
  draggedId.value = id
  dragOverId.value = null
  if (!event.dataTransfer) return
  event.dataTransfer.effectAllowed = 'move'
  try {
    event.dataTransfer.setData('text/plain', id)
  } catch {
    // Some browsers reject custom drag payloads; the in-memory id is enough.
  }
}

function handleDragOver(id: OverviewModuleId, event: DragEvent) {
  if (!draggedId.value) return
  // Without preventDefault the browser refuses the drop and shows the "no drop" cursor.
  event.preventDefault()
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'move'
  // Pointing back at the source must clear the previous target highlight.
  dragOverId.value = draggedId.value === id ? null : id
}

function handleDragLeave(id: OverviewModuleId, event: DragEvent) {
  if (dragOverId.value !== id) return
  const related = event.relatedTarget as Node | null
  if (related && (event.currentTarget as Node | null)?.contains(related)) return
  dragOverId.value = null
}

function handleDrop(id: OverviewModuleId, event: DragEvent) {
  event.preventDefault()
  event.stopPropagation()
  const source = draggedId.value
  clearDragState()
  if (!source || source === id) return
  emit('reorder', source, id)
}

/** Keeps the drop cursor valid when the pointer travels over the list background. */
function handleListDragOver(event: DragEvent) {
  if (!draggedId.value) return
  event.preventDefault()
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'move'
}

/** Dropping on the list background moves the module to the end of the list. */
function handleListDrop(event: DragEvent) {
  if (!draggedId.value) return
  event.preventDefault()
  const source = draggedId.value
  const last = props.modules[props.modules.length - 1]
  clearDragState()
  if (!last || source === last.id) return
  emit('reorder', source, last.id)
}

function handleListDragLeave(event: DragEvent) {
  const related = event.relatedTarget as Node | null
  if (related && (event.currentTarget as Node | null)?.contains(related)) return
  dragOverId.value = null
}

function close() {
  emit('update:open', false)
}
</script>

<template>
  <SettingsDialogShell
    :open="open"
    :title="t('设置仪表板')"
    :description="t('选择您想在页面显示的内容')"
    :close-label="t('关闭')"
    :show-description="true"
    content-class="overview-layout-dialog-content"
    @update:open="emit('update:open', $event)"
  >
    <template #icon>
      <SlidersHorizontal :size="18" />
    </template>

    <ul class="overview-layout-list" @dragover="handleListDragOver" @drop="handleListDrop" @dragleave="handleListDragLeave">
      <li
        v-for="(module, index) in modules"
        :key="module.id"
        class="overview-layout-item"
        :class="{
          'is-selected': !module.hidden,
          'is-dragging': draggedId === module.id,
          'is-drag-over': dragOverId === module.id,
        }"
        @dragover="handleDragOver(module.id, $event)"
        @dragleave="handleDragLeave(module.id, $event)"
        @drop="handleDrop(module.id, $event)"
      >
        <div class="overview-layout-item-main">
          <CheckboxField
            :model-value="!module.hidden"
            :label="t(module.label)"
            @update:model-value="emit('toggle', module.id)"
          />
        </div>
        <div class="overview-layout-item-actions">
          <button
            class="overview-layout-move-button"
            type="button"
            :aria-label="`${t('上移')} ${t(module.label)}`"
            :disabled="index === 0"
            @click="emit('move', module.id, -1)"
          >
            <ArrowUp :size="14" aria-hidden="true" />
          </button>
          <button
            class="overview-layout-move-button"
            type="button"
            :aria-label="`${t('下移')} ${t(module.label)}`"
            :disabled="index === modules.length - 1"
            @click="emit('move', module.id, 1)"
          >
            <ArrowDown :size="14" aria-hidden="true" />
          </button>
          <span
            class="overview-layout-item-handle"
            draggable="true"
            aria-hidden="true"
            :title="t('拖动排序')"
            @dragstart="handleDragStart(module.id, $event)"
            @dragend="clearDragState"
          >
            <GripVertical :size="16" aria-hidden="true" />
          </span>
        </div>
      </li>
    </ul>

    <div class="overview-layout-dialog-actions">
      <button
        class="secondary"
        type="button"
        :aria-label="t('恢复默认首页布局')"
        @click="emit('reset')"
      >
        {{ t('恢复默认') }}
      </button>
    </div>

    <template #footer>
      <button class="secondary" type="button" @click="close">{{ t('取消') }}</button>
      <button class="primary" type="button" @click="emit('save')">{{ t('保存') }}</button>
    </template>
  </SettingsDialogShell>
</template>
