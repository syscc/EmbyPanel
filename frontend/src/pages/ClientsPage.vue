<script setup lang="ts">
import {
  Check,
  Clock3,
  Plus,
  RefreshCw,
  ShieldCheck,
  Trash2,
  Users,
} from '@lucide/vue'
import { usePanelContext } from '@/composables/panel-context'

const {
  clientControl,
  clientControlError,
  savingClientControl,
  addingClientRule,
  clientStatusFilter,
  clientKeywordFilter,
  manualClientRule,
  rateLimitWindows,
  playbackLimitActionOptions,
  activeRateLimitBlocks,
  allowedClientCount,
  blockedClientCount,
  clientRuleRows,
  t,
  refreshClientControl,
  saveClientControl,
  refreshRateLimitStatus,
  unblockRateLimit,
  addClientRule,
  clearClientFilters,
  clientKeyword,
  deleteClientRule,
  toggleClientRule,
  formatIpLocation,
  formatServerName,
  formatTimestamp,
  playbackRateActionLabel,
  rateLimitBlockIp,
  rateLimitBlockReason,
} = usePanelContext()
</script>

<template>
  <section class="client-page">
    <section class="panel">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <Users :size="18" />
            <h2>{{ t('客户端管控') }}</h2>
          </div>
          <p class="muted">
            {{ t('自动记录播放设备和 UA，也可以按播放频率临时禁用账号。') }}
          </p>
        </div>
        <div class="panel-actions">
          <button class="secondary" @click="refreshClientControl">
            <RefreshCw :size="15" />{{ t('刷新') }}
          </button>
          <button
            class="primary"
            :disabled="savingClientControl"
            @click="saveClientControl"
          >
            <Check :size="15" />{{
              savingClientControl ? t('保存中') : t('保存')
            }}
          </button>
        </div>
      </div>
      <div v-if="clientControlError" class="notice error" role="alert">
        {{ clientControlError }}
      </div>
      <div class="client-toolbar">
        <label class="check">
          <input v-model="clientControl.enabled" type="checkbox" />
          <span>{{ t('启用 UA 拦截') }}</span>
        </label>
        <label class="check">
          <input
            v-model="clientControl.playback_rate_limit_enabled"
            type="checkbox"
          />
          <span>{{ t('启用播放频率限制') }}</span>
        </label>
        <label class="check">
          <input
            v-model="clientControl.concurrent_playback_limit_enabled"
            type="checkbox"
          />
          <span>{{ t('启用同时播放限制') }}</span>
        </label>
      </div>
      <div class="rate-limit-grid">
        <label>
          <span>{{ t('屏蔽方式') }}</span>
          <select v-model="clientControl.playback_rate_limit_action">
            <option
              v-for="option in playbackLimitActionOptions"
              :key="option.value"
              :value="option.value"
            >
              {{ t(option.label) }}
            </option>
          </select>
        </label>
        <label>
          <span>{{ t('检测时间窗口（秒）') }}</span>
          <input
            v-model.number="clientControl.playback_rate_limit_window_seconds"
            type="number"
            min="1"
          />
        </label>
        <label>
          <span>{{ t('最大播放次数') }}</span>
          <input
            v-model.number="clientControl.playback_rate_limit_max_requests"
            type="number"
            min="1"
          />
        </label>
        <label>
          <span>{{ t('封禁时长（秒）') }}</span>
          <input
            v-model.number="clientControl.playback_rate_limit_block_seconds"
            type="number"
            min="1"
          />
        </label>
        <label>
          <span>{{ t('允许同时播放数') }}</span>
          <input
            v-model.number="clientControl.concurrent_playback_limit_max"
            type="number"
            min="1"
            :disabled="!clientControl.concurrent_playback_limit_enabled"
          />
        </label>
      </div>
      <div class="rate-block-list">
        <div v-if="activeRateLimitBlocks.length" class="rate-block-table-wrap">
          <table class="rate-block-table client-rate-block-table">
            <thead>
              <tr>
                <th>{{ t('封禁方式') }}</th>
                <th>{{ t('封禁原因') }}</th>
                <th>{{ t('服务器') }}</th>
                <th>IP</th>
                <th>{{ t('用户') }}</th>
                <th>{{ t('到期时间') }}</th>
                <th>{{ activeRateLimitBlocks.length }} {{ t('条') }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="record in activeRateLimitBlocks" :key="record.id">
                <td>{{ playbackRateActionLabel(record.action) }}</td>
                <td>{{ rateLimitBlockReason(record) }}</td>
                <td>{{ record.server_name }}</td>
                <td>
                  <strong>{{ rateLimitBlockIp(record) }}</strong>
                  <small
                    v-if="formatIpLocation(record.ip_location)"
                    class="ip-location"
                    >{{ formatIpLocation(record.ip_location) }}</small
                  >
                </td>
                <td>{{ record.user_name || '--' }}</td>
                <td>{{ formatTimestamp(record.blocked_until) }}</td>
                <td>
                  <button class="secondary" @click="unblockRateLimit(record)">
                    <ShieldCheck :size="15" />{{ t('解除封禁') }}
                  </button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-else class="empty-state compact">
          {{ t('暂无频率限制封禁。') }}
        </div>
      </div>
    </section>

    <section class="panel">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <Clock3 :size="18" />
            <h2>{{ t('播放频率窗口') }}</h2>
          </div>
          <p class="muted">
            {{ t('显示当前检测窗口内各 IP 的播放请求计数。') }}
          </p>
        </div>
        <button class="secondary" @click="refreshRateLimitStatus">
          <RefreshCw :size="15" />{{ t('刷新') }}
        </button>
      </div>
      <div v-if="rateLimitWindows.length" class="rate-window-table-wrap">
        <table class="rate-window-table">
          <thead>
            <tr>
              <th>{{ t('服务器') }}</th>
              <th>IP</th>
              <th>{{ t('用户') }}</th>
              <th>{{ t('当前次数') }}</th>
              <th>{{ t('阈值') }}</th>
              <th>{{ t('剩余') }}</th>
              <th>{{ t('窗口') }}</th>
              <th>{{ t('重置时间') }}</th>
              <th>{{ t('状态') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="row in rateLimitWindows"
              :key="`${row.server_id}-${row.ip}`"
            >
              <td>{{ formatServerName(row.server_id) }}</td>
              <td>
                <strong>{{ row.ip }}</strong>
                <small
                  v-if="formatIpLocation(row.ip_location)"
                  class="ip-location"
                  >{{ formatIpLocation(row.ip_location) }}</small
                >
              </td>
              <td>{{ row.user_name || '--' }}</td>
              <td>{{ row.current_count }}</td>
              <td>{{ row.threshold }}</td>
              <td>{{ row.remaining }}</td>
              <td>{{ row.window_seconds }}s</td>
              <td>{{ formatTimestamp(row.reset_at) }}</td>
              <td>
                <span
                  :class="['client-badge', row.blocked ? 'blocked' : 'allowed']"
                >
                  {{ row.blocked ? t('已封禁') : t('观察中') }}
                </span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div v-else class="empty-state compact">
        {{ t('当前没有播放频率窗口数据。') }}
      </div>
    </section>

    <div class="client-filterbar client-filterbar-standalone">
      <button
        :class="['filter-button', { active: clientStatusFilter === 'all' }]"
        type="button"
        :aria-pressed="clientStatusFilter === 'all'"
        @click="clientStatusFilter = 'all'"
      >
        {{ t('全部') }} {{ clientControl.records.length }}
      </button>
      <button
        :class="['filter-button', { active: clientStatusFilter === 'blocked' }]"
        type="button"
        :aria-pressed="clientStatusFilter === 'blocked'"
        @click="clientStatusFilter = 'blocked'"
      >
        {{ t('已禁用') }} {{ blockedClientCount }}
      </button>
      <button
        :class="['filter-button', { active: clientStatusFilter === 'allowed' }]"
        type="button"
        :aria-pressed="clientStatusFilter === 'allowed'"
        @click="clientStatusFilter = 'allowed'"
      >
        {{ t('允许播放') }} {{ allowedClientCount }}
      </button>
      <input
        v-model="clientKeywordFilter"
        class="client-search"
        :aria-label="t('搜索 UA')"
        :placeholder="t('搜索 UA')"
      />
      <button
        class="secondary"
        :disabled="clientStatusFilter === 'all' && !clientKeywordFilter"
        @click="clearClientFilters"
      >
        {{ t('清空筛选') }}
      </button>
    </div>

    <section class="panel">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <Plus :size="18" />
            <h2>{{ t('手动添加 UA 拦截') }}</h2>
          </div>
          <p class="muted">
            {{ t('输入 UA 完整内容或关键字，保存后默认进入禁用状态。') }}
          </p>
        </div>
      </div>
      <div class="manual-rule-row">
        <label>
          <span>{{ t('UA 关键字') }}</span>
          <input
            v-model="manualClientRule.user_agent"
            :placeholder="t('例如：Infuse / Fileball / okhttp')"
            @keyup.enter="addClientRule"
          />
        </label>
        <label>
          <span>{{ t('描述') }}</span>
          <input
            v-model="manualClientRule.note"
            :placeholder="t('例如：临时禁用某客户端')"
            @keyup.enter="addClientRule"
          />
        </label>
        <button
          class="primary"
          :disabled="addingClientRule"
          @click="addClientRule"
        >
          <Plus :size="15" />{{
            addingClientRule ? t('添加中') : t('添加拦截')
          }}
        </button>
      </div>
    </section>

    <section class="panel client-table-panel">
      <div class="client-date-note">
        <strong>{{ t('日期说明') }}</strong>
        <span>{{
          t(
            '时间为该 UA 第一次出现或手动添加的时间；后台更新时间只在规则状态、备注或客户端信息变化时刷新，同一 UA 重复请求不会每次刷新。',
          )
        }}</span>
      </div>
      <div class="client-table-wrap">
        <table class="client-table">
          <thead>
            <tr>
              <th>{{ t('关键字') }}</th>
              <th>{{ t('描述') }}</th>
              <th>{{ t('状态') }}</th>
              <th>{{ t('记录时间') }}</th>
              <th>{{ t('操作') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="record in clientRuleRows" :key="record.id">
              <td>
                <strong :title="clientKeyword(record)">{{
                  clientKeyword(record)
                }}</strong>
              </td>
              <td>
                <span
                  :title="
                    record.note ||
                    (record.source === 'auto'
                      ? t('自动记录播放设备')
                      : t('手动 UA 拦截'))
                  "
                >
                  {{
                    record.note ||
                    (record.source === 'auto'
                      ? t('自动记录播放设备')
                      : t('手动 UA 拦截'))
                  }}
                </span>
              </td>
              <td>
                <span
                  :class="[
                    'client-badge',
                    record.enabled ? 'blocked' : 'allowed',
                  ]"
                >
                  {{ record.enabled ? t('已禁用') : t('允许播放') }}
                </span>
              </td>
              <td class="client-time-cell">
                {{ formatTimestamp(record.created_at) }}
              </td>
              <td>
                <div class="rule-actions">
                  <button
                    type="button"
                    :class="['switch-button', { active: record.enabled }]"
                    :aria-pressed="record.enabled"
                    :aria-label="
                      record.enabled ? t('关闭 UA 拦截') : t('开启 UA 拦截')
                    "
                    @click="toggleClientRule(record)"
                  >
                    <span />
                  </button>
                  <button
                    type="button"
                    class="danger-button"
                    @click="deleteClientRule(record)"
                  >
                    <Trash2 :size="15" />{{ t('删除') }}
                  </button>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
        <div v-if="!clientRuleRows.length" class="empty-state">
          {{
            clientControl.records.length
              ? t('当前筛选没有匹配的客户端。')
              : t(
                  '暂无客户端记录，开始播放后会自动出现，也可以手动添加 UA 拦截。',
                )
          }}
        </div>
      </div>
    </section>
  </section>
</template>
