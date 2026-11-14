<script setup lang="ts">
import {
  Check,
  Download,
  FileText,
  Settings2,
  SlidersHorizontal,
} from '@lucide/vue'
import { computed, unref } from 'vue'
import RefreshButton from '@/components/ui/RefreshButton.vue'
import { usePanelContext } from '@/composables/panel-context'
import { copyableLogDetail, formatLogDetail, logRequestId } from '@/lib/panel-formatters'
import type { PlaybackMedia } from '@/types/panel'

const {
  logsLoading,
  logsError,
  selectedLogServer,
  selectedLogView,
  selectedLogLevel,
  selectedRequestPathType,
  logKeywordFilter,
  logSince,
  logUntil,
  logConfig,
  canManageLogConfig,
  logConfigLoading,
  logConfigSaving,
  logConfigError,
  refreshLogConfig,
  activityLogMaxLimit,
  requestDetailDisplayMax,
  requestDetailPersistDays,
  requestDetailPersistMax,
  logServers,
  selectedLogViewLabel,
  visibleActivityLogRows,
  isRequestDetailLogView,
  showLogLevelFilter,
  logLevelOptions,
  filteredRequestDetailRows,
  visibleLogCount,
  canLoadMoreSelectedLogs,
  t,
  refreshActivityLogs,
  exportLogs,
  handleLogViewChange,
  refreshLogsWithReset,
  handleScrollableLogListScroll,
  loadMoreSelectedLogs,
  formatLogTime,
  logLevelLabel,
  ipWithLocation,
  isHttpUrl,
  copyText,
  requestOutcomeClass,
  requestSeverity,
  requestSeverityLabel,
  requestRowTitle,
  requestPathTypeLabel,
  saveLogConfig,
} = usePanelContext()

function mediaSummary(media?: PlaybackMedia | null) {
  const number = (prefix: string, value?: number | null) =>
    value != null && Number.isInteger(value) && value >= 0
      ? `${prefix}${String(value).padStart(2, '0')}` : ''
  const episode = number('S', media?.season_number) + number('E', media?.episode_number)
  return [media?.series_name, episode, media?.name].filter(Boolean).join(' · ') || t('媒体信息未知')
}

const displayActivityLogRows = computed(() =>
  unref(visibleActivityLogRows).map((entry) => ({
    ...entry,
    displayDetail: formatLogDetail(entry.detail),
    copyDetail: copyableLogDetail(entry.detail),
    requestId: logRequestId(entry.detail),
    mediaSummary: mediaSummary(entry.media),
  })),
)

const groupedActivityLogRows = computed(() => {
  type Entry = (typeof displayActivityLogRows.value)[number]
  type Row = { key: string; grouped: boolean; latest: Entry; requests: Entry[] }
  const rows: Row[] = []
  const groups = new Map<string, Row>()
  for (const entry of displayActivityLogRows.value) {
    const eligible = entry.kind === 'playback'
      && (entry.level === 'success' || entry.level === 'info')
      && typeof entry.playback_group_id === 'string' && entry.playback_group_id.length === 69
      && /^play-[0-9a-f]{64}$/.test(entry.playback_group_id)
      && typeof entry.server_id === 'string' && entry.server_id.trim() !== ''
    const key = eligible
      ? `playback-${JSON.stringify([entry.server_id, entry.playback_group_id])}`
      : `activity-${entry.id}`
    const existing = eligible ? groups.get(key) : undefined
    if (existing) {
      existing.requests.push(entry)
      if (entry.timestamp_ms > existing.latest.timestamp_ms) existing.latest = entry
    } else {
      const row = { key, grouped: Boolean(eligible), latest: entry, requests: [entry] }
      rows.push(row)
      if (eligible) groups.set(key, row)
    }
  }
  return rows
})

// The request-detail and block views read their detail from the proxy detail
// store, so the same formatting keeps every log view consistent.
const displayRequestDetailRows = computed(() =>
  unref(filteredRequestDetailRows).map((row) => ({
    ...row,
    displayDetail: formatLogDetail(row.detail),
    copyDetail: copyableLogDetail(row.detail),
    requestId: logRequestId(row.detail),
  })),
)
</script>

<template>
  <section class="logs-page">
    <section class="panel log-console-panel">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <FileText :size="18" />
            <h2>{{ t('日志') }}</h2>
          </div>
          <p class="muted">
            {{
              t(
                '单列表查看播放日志、拦截日志、反代请求和运行日志，页面打开时每 3 秒自动刷新。',
              )
            }}
          </p>
        </div>
        <div class="panel-actions">
          <span class="status-dot">{{
            logsLoading ? t('刷新中') : t('实时刷新')
          }}</span>
          <RefreshButton
            :label="t('刷新')"
            :loading-label="t('刷新中')"
            :action="refreshActivityLogs"
            :loading="logsLoading"
            :disabled="logsLoading"
          />
          <button
            v-if="!isRequestDetailLogView"
            class="secondary"
            @click="exportLogs"
          >
            <Download :size="15" />{{ t(selectedLogView === 'playback' ? '导出原始请求 CSV' : '导出 CSV') }}
          </button>
        </div>
      </div>
      <div v-if="logsError" class="notice error" role="alert">
        {{ logsError }}
      </div>
      <div
        :class="['log-toolbar', 'compact', { proxy: isRequestDetailLogView }]"
      >
        <label>
          <span>{{ t('日志类型') }}</span>
          <select v-model="selectedLogView" @change="handleLogViewChange">
            <option value="playback">{{ t('播放日志') }}</option>
            <option value="blocked">{{ t('拦截日志') }}</option>
            <option value="proxy">{{ t('反代请求') }}</option>
            <option value="general">{{ t('运行日志') }}</option>
          </select>
        </label>
        <label v-if="showLogLevelFilter">
          <span>{{ t('级别') }}</span>
          <select v-model="selectedLogLevel" @change="refreshLogsWithReset">
            <option
              v-for="option in logLevelOptions"
              :key="option.value"
              :value="option.value"
            >
              {{ t(option.label) }}
            </option>
          </select>
        </label>
        <label>
          <span>{{ t('服务器') }}</span>
          <select v-model="selectedLogServer" @change="refreshLogsWithReset">
            <option value="all">{{ t('全部服务器') }}</option>
            <option
              v-for="server in logServers"
              :key="server.id"
              :value="server.id"
            >
              {{ server.name }}
            </option>
          </select>
        </label>
        <label v-if="isRequestDetailLogView">
          <span>{{ t('请求类型') }}</span>
          <select
            v-model="selectedRequestPathType"
            @change="refreshLogsWithReset"
          >
            <option value="all">{{ t('全部请求') }}</option>
            <option value="video_stream">{{ t('视频流') }}</option>
            <option value="playback_info">{{ t('播放信息') }}</option>
            <option value="system_info">{{ t('系统信息') }}</option>
            <option value="base_html_player">{{ t('播放器脚本') }}</option>
            <option
              v-if="selectedLogView === 'blocked'"
              value="rate_limit_action"
            >
              {{ t('封禁动作') }}
            </option>
            <option value="proxy">{{ t('普通代理') }}</option>
          </select>
        </label>
        <label class="log-search-field">
          <span>{{ t('关键词') }}</span>
          <input
            v-model="logKeywordFilter"
            :placeholder="t('搜索用户 / IP / URL / 信息')"
            @keyup.enter="refreshLogsWithReset"
          />
        </label>
        <label>
          <span>{{ t('开始时间') }}</span>
          <input
            v-model="logSince"
            type="datetime-local"
            @change="refreshLogsWithReset"
          />
        </label>
        <label>
          <span>{{ t('结束时间') }}</span>
          <input
            v-model="logUntil"
            type="datetime-local"
            @change="refreshLogsWithReset"
          />
        </label>
        <div class="log-filter-actions">
          <button class="primary" @click="refreshLogsWithReset">
            <SlidersHorizontal :size="15" />{{ t('筛选') }}
          </button>
        </div>
      </div>

      <div class="log-console-meta">
        <span>{{ selectedLogViewLabel }}</span>
        <template v-if="selectedLogView === 'playback'">
          <span>{{ t('显示条目') }}</span><strong>{{ groupedActivityLogRows.length }}</strong>
          <span>{{ t('已加载原始请求') }}</span><strong>{{ displayActivityLogRows.length }}</strong>
        </template>
        <strong v-else>{{ visibleLogCount }}</strong>
        <span v-if="isRequestDetailLogView">
          {{ t('单次最多') }} {{ requestDetailDisplayMax }} {{ t('条') }}，{{
            t('保留')
          }}
          {{ requestDetailPersistDays }} {{ t('天或最近') }}
          {{ requestDetailPersistMax }} {{ t('条') }}
        </span>
        <span v-else
          >{{ t('内存最多保留最近') }} {{ activityLogMaxLimit }}
          {{ t('条可视化日志') }}</span
        >
      </div>
      <p v-if="selectedLogView === 'playback'" class="log-request-hint">
        {{ t('同一播放的多个 Range 分段请求折叠展示，展开可查看原始请求；警告和错误单独显示。') }}
        {{ t('数量仅包含当前已加载且匹配筛选的请求，不代表完整会话。') }}
        {{ t('CSV 按当前筛选导出最多 500 条原始请求，不按显示条目合并。') }}
      </p>
      <p v-if="selectedLogView === 'proxy'" class="log-request-hint">
        {{ t('播放请求按播放会话折叠展示在「播放日志」中；「反代请求」默认不列出播放流，选择「视频流」仍可查看原始播放请求。') }}
      </p>

      <div
        v-if="visibleLogCount"
        class="log-console-list"
        @scroll="handleScrollableLogListScroll($event, loadMoreSelectedLogs)"
      >
        <template v-if="!isRequestDetailLogView">
          <template v-for="row in groupedActivityLogRows" :key="row.key">
            <details v-if="row.grouped" class="log-playback-group">
              <summary class="log-group-summary">
                <span class="log-time">{{ t('最近请求') }}：{{ formatLogTime(row.latest.timestamp_ms) }}</span>
                <span class="log-body">
                  <span class="log-title">
                    <strong>{{ t('播放请求摘要') }}</strong>
                    <span class="server-pill">{{ row.latest.server_name }}</span>
                    <span v-if="row.latest.playback_user" class="user-pill">{{ row.latest.playback_user }}</span>
                  </span>
                  <span class="log-media" :title="row.latest.mediaSummary">{{ row.latest.mediaSummary }}</span>
                  <span class="log-group-count">{{ t('当前已加载请求数') }}：{{ row.requests.length }} · {{ t('展开查看原始请求') }}</span>
                </span>
              </summary>
              <div class="log-group-requests">
                <div v-for="entry in row.requests" :key="`request-${entry.id}`" class="log-group-request">
                  <div class="log-time">{{ formatLogTime(entry.timestamp_ms) }}</div>
                  <div class="log-body">
                    <div class="log-title">
                      <strong>{{ t(entry.message) }}</strong>
                      <span :class="['level-pill', entry.level]">{{ logLevelLabel(entry.level) }}</span>
                      <span class="server-pill">{{ entry.server_name }}</span>
                      <span v-if="entry.playback_user" class="user-pill">{{ entry.playback_user }}</span>
                      <span v-if="entry.playback_ip" class="ip-pill">{{ ipWithLocation(entry.playback_ip, entry.ip_location) }}</span>
                    </div>
                    <p class="log-media" :title="entry.mediaSummary">{{ entry.mediaSummary }}</p>
                    <p class="log-detail">
                      <a v-if="entry.displayDetail && isHttpUrl(entry.displayDetail)" class="copy-link" href="#"
                        :title="t('点击复制链接')" @click.prevent="copyText(entry.copyDetail)">{{ entry.displayDetail }}</a>
                      <template v-else>{{ entry.displayDetail || t('暂无详情') }}</template>
                    </p>
                    <p v-if="entry.requestId" class="log-request-id">{{ t('关联 ID') }}：<code>{{ entry.requestId }}</code></p>
                  </div>
                </div>
              </div>
            </details>
            <article v-else :class="['log-entry', row.latest.kind, row.latest.level]">
              <template v-for="entry in row.requests" :key="entry.id">
                <div class="log-time">{{ formatLogTime(entry.timestamp_ms) }}</div>
                <div class="log-body">
                  <div class="log-title">
                    <strong>{{ t(entry.message) }}</strong>
                    <span :class="['level-pill', entry.level]">{{
                      logLevelLabel(entry.level)
                    }}</span>
                    <span class="server-pill">{{ entry.server_name }}</span>
                    <span v-if="entry.playback_user" class="user-pill">{{
                      entry.playback_user
                    }}</span>
                    <span v-if="entry.playback_ip" class="ip-pill">{{
                      ipWithLocation(entry.playback_ip, entry.ip_location)
                    }}</span>
                  </div>
                  <p v-if="entry.kind === 'playback'" class="log-media" :title="entry.mediaSummary">
                    <span>{{ t('媒体') }}：</span>{{ entry.mediaSummary }}
                  </p>
                  <p :class="{ 'log-detail': entry.kind === 'playback' }">
                    <a
                      v-if="entry.displayDetail && isHttpUrl(entry.displayDetail)"
                      class="copy-link"
                      href="#"
                      :title="t('点击复制链接')"
                      @click.prevent="copyText(entry.copyDetail)"
                      >{{ entry.displayDetail }}</a
                    >
                    <template v-else>{{ entry.displayDetail || t('暂无详情') }}</template>
                  </p>
                  <p v-if="entry.requestId" class="log-request-id">
                    {{ t('关联 ID') }}：<code>{{ entry.requestId }}</code>
                  </p>
                </div>
              </template>
            </article>
          </template>
        </template>
        <template v-else>
          <article
            v-for="row in displayRequestDetailRows"
            :key="`proxy-${row.id}`"
            :class="['request-detail-row', requestOutcomeClass(row)]"
          >
            <div class="request-main">
              <div class="request-meta">
                <span :class="['level-pill', requestSeverity(row)]">{{
                  requestSeverityLabel(row)
                }}</span>
                <span class="server-pill">{{ row.server_name }}</span>
                <span class="user-pill">{{ row.playback_user || '--' }}</span>
                <span class="ip-pill">{{
                  ipWithLocation(row.playback_ip, row.ip_location)
                }}</span>
                <span class="request-time">{{
                  formatLogTime(row.timestamp_ms)
                }}</span>
              </div>
              <div class="request-path">
                <strong>{{ requestRowTitle(row) }}</strong>
                <a
                  v-if="row.displayDetail && isHttpUrl(row.displayDetail)"
                  class="copy-link"
                  href="#"
                  :title="t('点击复制链接')"
                  @click.prevent="copyText(row.copyDetail)"
                  >{{ row.displayDetail }}</a
                >
                <template v-else
                  ><span>{{ row.displayDetail || t('暂无详情') }}</span></template
                >
              </div>
              <p v-if="row.requestId" class="log-request-id">
                {{ t('关联 ID') }}：<code>{{ row.requestId }}</code>
              </p>
            </div>
            <div class="request-state">
              <span>{{ row.outcome }}</span>
              <span>{{ requestPathTypeLabel(row.path_type) }}</span>
              <span
                v-if="row.status_code === 0 && row.event_type !== 'block' && row.event_type !== 'unblock'"
              >{{ t('未返回响应') }}</span>
              <span
                v-if="
                  row.status_code !== 0 && row.event_type !== 'block' && row.event_type !== 'unblock'
                "
                >HTTP {{ row.status_code }}</span
              >
              <span
                v-if="
                  row.event_type !== 'block' && row.event_type !== 'unblock'
                "
                >{{ row.duration_ms }}ms</span
              >
              <span
                v-if="
                  row.event_type !== 'block' && row.event_type !== 'unblock'
                "
                >{{ row.cache_hit ? t('缓存命中') : t('未命中') }}</span
              >
              <span>{{
                row.event_type === 'unblock'
                  ? t('已解除')
                  : row.blocked
                    ? t('已拦截')
                    : t('未拦截')
              }}</span>
            </div>
          </article>
        </template>

        <button
          v-if="canLoadMoreSelectedLogs"
          class="load-more-row"
          :disabled="logsLoading"
          @click="loadMoreSelectedLogs"
        >
          {{
            logsLoading
              ? t('加载中')
              : `${t('加载更多')} ${selectedLogViewLabel}`
          }}
        </button>
        <div v-if="selectedLogView === 'playback'" class="log-limit-note">
          {{ t('显示条目') }}：{{ groupedActivityLogRows.length }} ·
          {{ t('已加载原始请求') }}：{{ displayActivityLogRows.length }}
        </div>
        <div v-else-if="!canLoadMoreSelectedLogs" class="log-limit-note">
          {{ t('已显示') }} {{ visibleLogCount }} {{ t('条') }}
          {{ selectedLogViewLabel }}
        </div>
      </div>
      <div v-else class="empty-state">
        {{ t('暂无') }} {{ selectedLogViewLabel }}
      </div>
    </section>

    <section v-if="canManageLogConfig" class="panel" :aria-busy="logConfigLoading || logConfigSaving">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <Settings2 :size="18" />
            <h2>{{ t('日志文件配置') }}</h2>
          </div>
          <p class="muted">
            {{ t('日志写入 data/logs/embypanel.log，默认 INFO 级别。') }}
          </p>
        </div>
        <div class="panel-actions">
          <RefreshButton
            :label="t('刷新')"
            :loading-label="t('刷新中')"
            :action="refreshLogConfig"
            :loading="logConfigLoading"
            :disabled="logConfigLoading || logConfigSaving"
          />
          <button class="primary" :disabled="logConfigLoading || logConfigSaving" @click="saveLogConfig">
            <Check :size="15" />{{ t('保存日志配置') }}
          </button>
        </div>
      </div>
      <div v-if="logConfigError" class="notice error" role="alert">{{ logConfigError }}</div>
      <div class="grid log-config-grid">
        <label>
          <span>{{ t('日志级别') }}</span>
          <select v-model="logConfig.level">
            <option value="debug">{{ t('DEBUG - 调试') }}</option>
            <option value="info">{{ t('INFO - 信息') }}</option>
            <option value="warning">{{ t('WARNING - 警告') }}</option>
            <option value="error">{{ t('ERROR - 错误') }}</option>
            <option value="critical">{{ t('CRITICAL - 严重') }}</option>
          </select>
        </label>
        <label>
          <span>{{ t('单文件最大 MB') }}</span>
          <input
            v-model.number="logConfig.max_size_mb"
            type="number"
            min="1"
            max="1024"
          />
        </label>
        <label>
          <span>{{ t('保留文件数') }}</span>
          <input
            v-model.number="logConfig.max_backups"
            type="number"
            min="1"
            max="99"
          />
        </label>
        <label class="log-format-field">
          <span>{{ t('日志格式') }}</span>
          <input v-model="logConfig.format" />
        </label>
      </div>
    </section>
  </section>
</template>
