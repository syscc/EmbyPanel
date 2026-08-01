<script setup lang="ts">
import {
  Check,
  Download,
  FileText,
  RefreshCw,
  Settings2,
  SlidersHorizontal,
} from '@lucide/vue'
import { usePanelContext } from '@/composables/panel-context'

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
          <button
            class="secondary"
            :disabled="logsLoading"
            @click="refreshActivityLogs"
          >
            <RefreshCw :size="15" />{{ logsLoading ? t('刷新中') : t('刷新') }}
          </button>
          <button
            v-if="!isRequestDetailLogView"
            class="secondary"
            @click="exportLogs"
          >
            <Download :size="15" />{{ t('导出 CSV') }}
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
              {{ server.name }} · :{{ server.port }}
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
        <strong>{{ visibleLogCount }}</strong>
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

      <div
        v-if="visibleLogCount"
        class="log-console-list"
        @scroll="handleScrollableLogListScroll($event, loadMoreSelectedLogs)"
      >
        <template v-if="!isRequestDetailLogView">
          <article
            v-for="entry in visibleActivityLogRows"
            :key="`activity-${entry.id}`"
            :class="['log-entry', entry.kind, entry.level]"
          >
            <div class="log-time">{{ formatLogTime(entry.timestamp_ms) }}</div>
            <div class="log-body">
              <div class="log-title">
                <strong>{{ entry.message }}</strong>
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
              <p :class="{ 'log-detail': entry.kind === 'playback' }">
                <a
                  v-if="entry.detail && isHttpUrl(entry.detail)"
                  class="copy-link"
                  href="#"
                  :title="t('点击复制链接')"
                  @click.prevent="copyText(entry.detail)"
                  >{{ entry.detail }}</a
                >
                <template v-else>{{ entry.detail || t('暂无详情') }}</template>
              </p>
            </div>
          </article>
        </template>
        <template v-else>
          <article
            v-for="row in filteredRequestDetailRows"
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
                <span>{{ row.detail }}</span>
              </div>
            </div>
            <div class="request-state">
              <span>{{ row.outcome }}</span>
              <span>{{ requestPathTypeLabel(row.path_type) }}</span>
              <span
                v-if="
                  row.event_type !== 'block' && row.event_type !== 'unblock'
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
        <div v-else class="log-limit-note">
          {{ t('已显示') }} {{ visibleLogCount }} {{ t('条') }}
          {{ selectedLogViewLabel }}
        </div>
      </div>
      <div v-else class="empty-state">
        {{ t('暂无') }} {{ selectedLogViewLabel }}
      </div>
    </section>

    <section class="panel">
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
        <button class="primary" @click="saveLogConfig">
          <Check :size="15" />{{ t('保存日志配置') }}
        </button>
      </div>
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
