<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { useDiagnosticsController, type DiagnosisRecord } from '@/composables/useDiagnosticsController'
import { usePanelContext } from '@/composables/panel-context'
import SettingsDialogShell from '@/components/ui/SettingsDialogShell.vue'

const {
  t,
  showNotice,
  clearNotice,
  api,
  confirmAction,
} = usePanelContext()

const controller = useDiagnosticsController({
  api,
  translate: t,
  showNotice,
  clearNotice,
  confirmAction,
})

const {
  diagnostics,
  loading,
  cleaning,
  canGlobalCleanup,
  error,
  query,
  hasNextPage,
  loadDiagnostics,
  applyFilters,
  resetFilters,
  nextPage,
  prevPage,
  cleanDiagnostics,
  dispose,
} = controller

const selectedRecord = ref<DiagnosisRecord | null>(null)
const detailOpen = ref(false)

function openDetail(record: DiagnosisRecord) {
  selectedRecord.value = record
  detailOpen.value = true
}

function onDetailOpenChange(open: boolean) {
  detailOpen.value = open
  if (!open) selectedRecord.value = null
}

onMounted(() => {
  void loadDiagnostics()
})

onUnmounted(() => {
  dispose()
})
</script>

<template>
  <div class="diagnostics-page">
    <div class="page-header">
      <h1>{{ t('诊断') }}</h1>
      <div class="header-actions">
        <button type="button" class="btn" :disabled="loading || cleaning" @click="() => loadDiagnostics()">
          {{ t('刷新') }}
        </button>
        <button
          v-if="canGlobalCleanup"
          type="button"
          class="btn danger"
          :disabled="!canGlobalCleanup || loading || cleaning"
          @click="cleanDiagnostics"
        >
          {{ cleaning ? t('处理中') : t('清空诊断记录') }}
        </button>
      </div>
    </div>

    <form class="filters" @submit.prevent="applyFilters">
      <label>
        <span>{{ t('请求 ID') }}</span>
        <input v-model="query.request_id" type="text" autocomplete="off" />
      </label>
      <label>
        <span>{{ t('服务器 ID') }}</span>
        <input v-model="query.server_id" type="text" autocomplete="off" />
      </label>
      <label>
        <span>{{ t('媒体 ID') }}</span>
        <input v-model="query.media_id" type="text" autocomplete="off" />
      </label>
      <label>
        <span>{{ t('每页') }}</span>
        <input v-model.number="query.limit" type="number" min="1" max="100" />
      </label>
      <label>
        <span>{{ t('页码') }}</span>
        <input v-model.number="query.page" type="number" min="1" />
      </label>
      <div class="filter-actions">
        <button type="submit" class="btn" :disabled="loading || cleaning">{{ t('筛选') }}</button>
        <button type="button" class="btn ghost" :disabled="loading || cleaning" @click="resetFilters">
          {{ t('重置') }}
        </button>
      </div>
    </form>

    <div v-if="loading" class="state" role="status" aria-live="polite" aria-busy="true">
      {{ t('加载中') }}
    </div>
    <div v-else-if="error" class="state error" role="alert">{{ error }}</div>
    <div v-else-if="diagnostics.length === 0" class="state">{{ t('暂无诊断记录') }}</div>
    <div v-else class="record-list">
      <article
        v-for="record in diagnostics"
        :key="record.request_id"
        class="record-card"
      >
        <header class="record-head">
          <div>
            <h2>{{ record.request_id }}</h2>
            <p class="meta">
              <span>{{ t('服务器 ID') }}: {{ record.server_id }}</span>
              <span>{{ t('媒体 ID') }}: {{ record.media_id }}</span>
            </p>
          </div>
          <div class="badges">
            <span class="badge" :class="{ hit: record.cache_hit }">
              {{ record.cache_hit ? t('缓存命中') : t('未命中') }}
            </span>
            <span v-if="record.failure_node" class="badge fail">
              {{ t('失败节点') }}: {{ t(record.failure_node) }}
            </span>
          </div>
        </header>

        <p class="ua">{{ t('用户代理') }}: {{ record.ua }}</p>

        <ul v-if="record.steps.length" class="steps">
          <li v-for="(step, index) in record.steps" :key="`${record.request_id}-${index}-${step.name}`">
            <strong>{{ t(step.name) }}</strong>
            <span>{{ t('耗时') }}: {{ step.duration_ms }} ms</span>
            <p>{{ t(step.details) }}</p>
          </li>
        </ul>

        <ul v-if="record.sanitized_errors.length" class="errors">
          <li v-for="(item, index) in record.sanitized_errors" :key="`${record.request_id}-err-${index}`">
            {{ t('错误') }}: {{ t(item) }}
          </li>
        </ul>

        <button type="button" class="btn ghost" @click="openDetail(record)">
          {{ t('查看详情') }}
        </button>
      </article>
    </div>

    <div v-if="!loading && !error" class="pager">
      <button type="button" class="btn ghost" :disabled="query.page <= 1 || cleaning" @click="prevPage">
        {{ t('上一页') }}
      </button>
      <span>{{ t('页码') }}: {{ query.page }} · {{ diagnostics.length }} {{ t('条') }}</span>
      <button type="button" class="btn ghost" :disabled="!hasNextPage || cleaning" @click="nextPage">
        {{ t('下一页') }}
      </button>
    </div>

    <SettingsDialogShell
      :open="detailOpen"
      :title="selectedRecord?.request_id || t('诊断')"
      :description="t('诊断步骤')"
      :close-label="t('关闭')"
      :show-description="true"
      @update:open="onDetailOpenChange"
    >
      <div v-if="selectedRecord" class="detail-body">
        <p><strong>{{ t('请求 ID') }}:</strong> {{ selectedRecord.request_id }}</p>
        <p><strong>{{ t('服务器 ID') }}:</strong> {{ selectedRecord.server_id }}</p>
        <p><strong>{{ t('媒体 ID') }}:</strong> {{ selectedRecord.media_id }}</p>
        <p><strong>{{ t('用户代理') }}:</strong> {{ selectedRecord.ua }}</p>
        <p>
          <strong>{{ t('缓存命中') }}:</strong>
          {{ selectedRecord.cache_hit ? t('命中') : t('未命中') }}
        </p>
        <p v-if="selectedRecord.failure_node">
          <strong>{{ t('失败节点') }}:</strong> {{ t(selectedRecord.failure_node) }}
        </p>

        <section v-if="selectedRecord.steps.length" class="detail-section">
          <h3>{{ t('诊断步骤') }}</h3>
          <article
            v-for="(step, index) in selectedRecord.steps"
            :key="`detail-step-${index}-${step.name}`"
            class="detail-step"
          >
            <h4>{{ t(step.name) }}</h4>
            <p>{{ t('耗时') }}: {{ step.duration_ms }} ms</p>
            <p>{{ t(step.details) }}</p>
          </article>
        </section>

        <section v-if="selectedRecord.sanitized_errors.length" class="detail-section">
          <h3>{{ t('错误') }}</h3>
          <ul>
            <li
              v-for="(item, index) in selectedRecord.sanitized_errors"
              :key="`detail-err-${index}`"
            >
              {{ t(item) }}
            </li>
          </ul>
        </section>
      </div>
    </SettingsDialogShell>
  </div>
</template>

<style scoped>
.diagnostics-page {
  display: flex;
  flex-direction: column;
  gap: 1rem;
  padding: 1rem;
  min-width: 0;
}

.page-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 1rem;
  flex-wrap: wrap;
}

.header-actions,
.filter-actions,
.pager,
.badges {
  display: flex;
  gap: 0.5rem;
  align-items: center;
  flex-wrap: wrap;
}

.filters {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
  gap: 0.75rem;
  padding: 0.75rem;
  border: 1px solid var(--border, #3333);
  border-radius: 8px;
}

.filters label {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
  font-size: 0.875rem;
}

.filters input {
  border: 1px solid var(--border, #3333);
  border-radius: 6px;
  padding: 0.4rem 0.55rem;
  background: transparent;
}

.btn {
  border: 1px solid var(--border, #3333);
  border-radius: 6px;
  padding: 0.4rem 0.75rem;
  background: var(--bg, transparent);
  cursor: pointer;
}

.btn:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.btn.danger {
  border-color: #b91c1c;
  color: #b91c1c;
}

.btn.ghost {
  background: transparent;
}

.state {
  padding: 1.5rem;
  text-align: center;
  border: 1px dashed var(--border, #3333);
  border-radius: 8px;
}

.state.error {
  color: #b91c1c;
  border-style: solid;
}

.record-list {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
}

.record-card {
  border: 1px solid var(--border, #3333);
  border-radius: 8px;
  padding: 0.9rem;
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
  min-width: 0;
}

.record-head {
  display: flex;
  justify-content: space-between;
  gap: 0.75rem;
  flex-wrap: wrap;
}

.record-head h2 {
  margin: 0;
  font-size: 1rem;
}

.meta {
  margin: 0.25rem 0 0;
  display: flex;
  flex-wrap: wrap;
  gap: 0.75rem;
  opacity: 0.8;
  font-size: 0.875rem;
}

.badge {
  border: 1px solid var(--border, #3333);
  border-radius: 999px;
  padding: 0.15rem 0.55rem;
  font-size: 0.75rem;
}

.badge.hit {
  border-color: #15803d;
  color: #15803d;
}

.badge.fail {
  border-color: #b91c1c;
  color: #b91c1c;
}

.ua,
.steps,
.errors,
.detail-body {
  margin: 0;
  font-size: 0.9rem;
  min-width: 0;
  overflow-wrap: anywhere;
}

.steps,
.errors,
.detail-body ul {
  padding-left: 1.1rem;
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
}

.detail-section {
  margin-top: 1rem;
}

.detail-step {
  border: 1px solid var(--border, #3333);
  border-radius: 6px;
  padding: 0.6rem;
  margin-top: 0.5rem;
  min-width: 0;
  overflow-wrap: anywhere;
}

.detail-step h4 {
  margin: 0 0 0.35rem;
}

.pager {
  justify-content: space-between;
}

@media (max-width: 480px) {
  .diagnostics-page {
    padding: 0.75rem;
  }

  .filters {
    grid-template-columns: minmax(0, 1fr);
  }

  .filters input,
  .filter-actions,
  .filter-actions .btn,
  .record-card > .btn {
    width: 100%;
  }

  .pager {
    align-items: stretch;
  }
}
</style>
