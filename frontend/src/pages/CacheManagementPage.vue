<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { RefreshCw, Search, Trash2 } from '@lucide/vue'
import { useCacheManagementController } from '@/composables/useCacheManagementController'
import { usePanelContext } from '@/composables/panel-context'

const panel = usePanelContext()
const { t, showNotice, api, confirmAction } = panel

const serverNames = computed(() => new Map(
  panel.serverOptions.value.map((server) => [server.id, server.name.trim()]),
))

function serverName(id: string) {
  if (!id) return '—'
  return serverNames.value.get(id) || id
}

const {
  stats,
  canViewGlobalStats,
  canCleanupAll,
  entries,
  loading,
  cleaning,
  error,
  search,
  serverId,
  mediaId,
  page,
  hasNextPage,
  canNextPage,
  hitRatePercent,
  canCleanupServer,
  canCleanupMedia,
  canCleanupServerMedia,
  refresh,
  applySearch,
  nextPage,
  prevPage,
  cleanupAll,
  cleanupByServer,
  cleanupByMedia,
  cleanupByServerMedia,
  dispose,
} = useCacheManagementController({
  api,
  translate: t,
  showNotice,
  confirmAction,
})

const refreshing = ref(false)
let disposed = false

async function refreshPage() {
  if (disposed || refreshing.value || loading.value || cleaning.value) return
  refreshing.value = true
  error.value = ''
  try {
    if (!await panel.refreshServerOptions() || disposed) return
    await refresh()
  } catch (cause) {
    if (disposed || (cause instanceof Error && cause.name === 'AbortError')) return
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    refreshing.value = false
  }
}

onMounted(() => {
  void refresh()
})

onBeforeUnmount(() => {
  disposed = true
  dispose()
})
</script>

<template>
  <section class="cache-page">
    <section class="panel">
      <div class="panel-head">
        <div>
          <h2>{{ t('缓存管理') }}</h2>
          <p class="muted">
            {{ t('查看直链缓存统计与条目，并按后端支持的范围清理。') }}
          </p>
        </div>
        <div class="panel-actions">
          <button
            type="button"
            class="secondary"
            :disabled="loading || refreshing || cleaning"
            @click="refreshPage"
          >
            <RefreshCw :size="15" />
            {{ loading || refreshing ? t('加载中') : t('刷新') }}
          </button>
          <button
            v-if="canCleanupAll"
            type="button"
            class="danger-button"
            :disabled="loading || refreshing || cleaning"
            @click="cleanupAll"
          >
            <Trash2 :size="15" />
            {{ cleaning ? t('处理中') : t('清理全部缓存') }}
          </button>
        </div>
      </div>

      <div v-if="error" class="notice error" role="alert">{{ error }}</div>

      <div v-if="canViewGlobalStats" class="cache-stats" aria-live="polite">
        <div class="cache-stat">
          <span class="cache-stat-label">{{ t('当前条目') }}</span>
          <strong>{{ stats.capacity }} {{ t('条') }}</strong>
        </div>
        <div class="cache-stat">
          <span class="cache-stat-label">{{ t('最大容量') }}</span>
          <strong>{{ stats.maxCapacity }} {{ t('条') }}</strong>
        </div>
        <div class="cache-stat">
          <span class="cache-stat-label">{{ t('命中') }}</span>
          <strong>{{ stats.hits.toLocaleString() }}</strong>
        </div>
        <div class="cache-stat">
          <span class="cache-stat-label">{{ t('未命中') }}</span>
          <strong>{{ stats.misses.toLocaleString() }}</strong>
        </div>
        <div class="cache-stat">
          <span class="cache-stat-label">{{ t('命中率') }}</span>
          <strong>{{ hitRatePercent }}</strong>
        </div>
        <div class="cache-stat">
          <span class="cache-stat-label">{{ t('容量单位') }}</span>
          <strong>{{ stats.unit === 'entries' ? t('条') : stats.unit }}</strong>
        </div>
      </div>
    </section>

    <section class="panel">
      <div class="panel-head compact">
        <div>
          <h3>{{ t('筛选与清理') }}</h3>
          <p class="muted">
            {{ t('搜索只过滤列表；清理必须选择后端支持的明确范围。') }}
          </p>
        </div>
      </div>

      <div class="cache-toolbar">
        <label class="cache-field">
          <span>{{ t('搜索') }}</span>
          <input
            v-model="search"
            type="search"
            maxlength="256"
            :placeholder="t('搜索 ID / 服务器 / 媒体 / 来源')"
            :disabled="loading || refreshing || cleaning"
            @keydown.enter.prevent="applySearch"
          />
        </label>
        <button
          type="button"
          class="secondary"
          :disabled="loading || refreshing || cleaning"
          @click="applySearch"
        >
          <Search :size="15" />
          {{ t('搜索') }}
        </button>
      </div>

      <div class="cache-toolbar scoped">
        <label class="cache-field">
          <span>{{ t('服务器 ID') }}</span>
          <input
            v-model="serverId"
            type="text"
            maxlength="128"
            :placeholder="t('服务器 ID')"
            :disabled="loading || refreshing || cleaning"
            autocomplete="off"
          />
        </label>
        <label class="cache-field">
          <span>{{ t('媒体 ID') }}</span>
          <input
            v-model="mediaId"
            type="text"
            maxlength="128"
            :placeholder="t('媒体 ID')"
            :disabled="loading || refreshing || cleaning"
            autocomplete="off"
          />
        </label>
        <div class="cache-scope-actions">
          <button
            type="button"
            class="secondary"
            :disabled="loading || refreshing || cleaning || !canCleanupServer"
            @click="cleanupByServer"
          >
            {{ t('按服务器清理') }}
          </button>
          <button
            type="button"
            class="secondary"
            :disabled="loading || refreshing || cleaning || !canCleanupMedia"
            @click="cleanupByMedia"
          >
            {{ t('按媒体清理') }}
          </button>
          <button
            type="button"
            class="secondary"
            :disabled="loading || refreshing || cleaning || !canCleanupServerMedia"
            @click="cleanupByServerMedia"
          >
            {{ t('按服务器和媒体清理') }}
          </button>
        </div>
      </div>
    </section>

    <section class="panel">
      <div class="panel-head compact">
        <div>
          <h3>{{ t('缓存条目') }}</h3>
          <p class="muted">{{ t('最多显示 500 条') }}</p>
        </div>
        <div class="panel-actions">
          <span class="muted">{{ entries.length }} {{ t('条') }}</span>
        </div>
      </div>

      <div v-if="loading && !entries.length" class="empty-state compact">
        {{ t('加载中') }}
      </div>
      <div v-else-if="!entries.length" class="empty-state compact">
        {{ t('没有缓存条目') }}
      </div>
      <div v-else class="cache-table-wrap">
        <table class="cache-table">
          <thead>
            <tr>
              <th :title="t('用于区分缓存记录，媒体请查看媒体 ID。')">{{ t('缓存条目 ID') }}</th>
              <th>{{ t('服务器') }}</th>
              <th>{{ t('媒体 ID') }}</th>
              <th>{{ t('来源') }}</th>
              <th>{{ t('状态') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="entry in entries" :key="entry.id">
              <td :data-label="t('缓存条目 ID')">
                <code :title="entry.id">{{ entry.id }}</code>
              </td>
              <td :data-label="t('服务器')" :title="entry.serverId || undefined">
                {{ serverName(entry.serverId) }}
              </td>
              <td :data-label="t('媒体 ID')">
                {{ entry.mediaId || '—' }}
              </td>
              <td :data-label="t('来源')">
                {{ entry.sourceId || '—' }}
              </td>
              <td :data-label="t('状态')">
                {{ entry.legacy ? t('旧格式') : t('可解析') }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div v-if="page > 1 || hasNextPage" class="cache-pagination" aria-live="polite">
        <button
          type="button"
          class="secondary"
          :disabled="loading || refreshing || cleaning || page <= 1"
          @click="prevPage"
        >
          {{ t('上一页') }}
        </button>
        <span class="muted">{{ t('页码') }} {{ page }}</span>
        <button
          type="button"
          class="secondary"
          :disabled="loading || refreshing || cleaning || !canNextPage"
          @click="nextPage"
        >
          {{ t('下一页') }}
        </button>
      </div>
    </section>
  </section>
</template>

<style scoped>
.cache-page {
  display: grid;
  gap: 16px;
}

.cache-pagination {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 12px;
  margin-top: 14px;
}

.cache-stats {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
  gap: 12px;
}

.cache-stat {
  display: grid;
  gap: 6px;
  padding: 12px 14px;
  border: 1px solid var(--border, rgba(127, 127, 127, 0.25));
  border-radius: 10px;
  background: var(--panel-soft, transparent);
}

.cache-stat-label {
  color: var(--muted, #888);
  font-size: 0.85rem;
}

.cache-toolbar {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  align-items: end;
  margin-bottom: 12px;
}

.cache-toolbar.scoped {
  margin-bottom: 0;
}

.cache-field {
  display: grid;
  gap: 6px;
  min-width: min(240px, 100%);
  flex: 1;
}

.cache-field span {
  color: var(--muted, #888);
  font-size: 0.85rem;
}

.cache-field input {
  width: 100%;
}

.cache-scope-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
}

.cache-table-wrap {
  overflow-x: auto;
}

.cache-table {
  width: 100%;
  border-collapse: collapse;
}

.cache-table th,
.cache-table td {
  padding: 10px 12px;
  text-align: left;
  border-bottom: 1px solid var(--border, rgba(127, 127, 127, 0.2));
  vertical-align: top;
}

.cache-table code {
  font-size: 0.85em;
  word-break: break-all;
}

@media (max-width: 720px) {
  .cache-table thead {
    display: none;
  }

  .cache-table tr {
    display: grid;
    gap: 6px;
    padding: 12px 0;
    border-bottom: 1px solid var(--border, rgba(127, 127, 127, 0.2));
  }

  .cache-table td {
    display: grid;
    grid-template-columns: 96px 1fr;
    gap: 8px;
    border: 0;
    padding: 0;
  }

  .cache-table td::before {
    content: attr(data-label);
    color: var(--muted, #888);
  }
}
</style>
