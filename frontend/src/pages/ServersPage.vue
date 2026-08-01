<script setup lang="ts">
import {
  Check,
  ChevronDown,
  ChevronRight,
  Eye,
  EyeOff,
  Plus,
  RefreshCw,
  RotateCw,
  Server,
  ShieldCheck,
  Trash2,
  Zap,
} from '@lucide/vue'
import { usePanelContext } from '@/composables/panel-context'

const {
  settings,
  saving,
  restartingServerId,
  validationResults,
  defaultCdnHeaders,
  realIpModeOptions,
  proxyStatusById,
  strmMappingPlaceholder,
  t,
  addServer,
  validateSettings,
  saveSettings,
  isServerExpanded,
  toggleServerExpanded,
  toggleProxyServer,
  restartProxyServer,
  removeServer,
  proxyStatusLabel,
  formatTimestampMs,
  isApiKeyVisible,
  toggleApiKeyVisible,
  needsRealIpHeader,
  updateRealIpMode,
  validationClass,
  localizeValidationText,
} = usePanelContext()
</script>

<template>
  <form class="panel" autocomplete="off" @submit.prevent="saveSettings">
    <div class="panel-head">
      <div>
        <div class="panel-title-line">
          <Server :size="18" />
          <h2>{{ t('服务器配置') }}</h2>
        </div>
        <p class="muted">
          {{ t('每个 Emby 服务器使用独立反代端口，保存后自动监听。') }}
        </p>
      </div>
      <div class="panel-actions">
        <button class="secondary" type="button" @click="addServer">
          <Plus :size="15" />{{ t('添加服务器') }}
        </button>
        <button
          class="secondary"
          type="button"
          :disabled="saving"
          @click="validateSettings"
        >
          <ShieldCheck :size="15" />{{ t('测试配置') }}
        </button>
        <button class="primary" type="submit" :disabled="saving">
          <Check :size="15" />{{ saving ? t('保存中') : t('保存配置') }}
        </button>
      </div>
    </div>
    <div class="server-list">
      <article
        v-for="(server, index) in settings.servers"
        :key="server.id"
        class="server-card"
      >
        <div class="server-card-head">
          <strong>{{ server.name || `${t('服务器')} ${index + 1}` }}</strong>
          <div class="server-actions">
            <button
              type="button"
              class="secondary"
              :aria-expanded="isServerExpanded(server.id)"
              @click="toggleServerExpanded(server.id)"
            >
              <ChevronDown v-if="!isServerExpanded(server.id)" :size="15" />
              <ChevronRight v-else :size="15" />
              {{ isServerExpanded(server.id) ? t('收起配置') : t('展开配置') }}
            </button>
            <button
              type="button"
              :class="['server-toggle-button', { disabled: !server.enabled }]"
              :disabled="saving || restartingServerId === server.id"
              :aria-pressed="server.enabled"
              @click="toggleProxyServer(server)"
            >
              <Zap :size="15" />{{
                server.enabled ? t('关闭服务器') : t('开启服务器')
              }}
            </button>
            <button
              type="button"
              class="secondary restart-button"
              :disabled="
                saving || restartingServerId === server.id || !server.enabled
              "
              @click="restartProxyServer(server)"
            >
              <RotateCw :size="15" />{{
                restartingServerId === server.id ? t('重启中') : t('重启服务器')
              }}
            </button>
            <button
              class="danger-button"
              type="button"
              @click="removeServer(server.id)"
            >
              <Trash2 :size="15" />{{ t('删除') }}
            </button>
          </div>
        </div>
        <div class="server-status-strip">
          <span
            :class="[
              'client-badge',
              proxyStatusById[server.id]?.listening ? 'allowed' : 'blocked',
            ]"
          >
            {{ proxyStatusLabel(proxyStatusById[server.id]) }}
          </span>
          <span
            >{{ t('端口') }} :{{
              proxyStatusById[server.id]?.port || server.port
            }}</span
          >
          <span
            >{{ t('启动') }}
            {{
              formatTimestampMs(proxyStatusById[server.id]?.started_at_ms)
            }}</span
          >
          <span
            >{{ t('最近请求') }}
            {{
              formatTimestampMs(proxyStatusById[server.id]?.last_request_ms)
            }}</span
          >
          <span
            v-if="proxyStatusById[server.id]?.last_error"
            class="server-status-error"
          >
            {{ proxyStatusById[server.id]?.last_error }}
          </span>
        </div>
        <div v-if="isServerExpanded(server.id)" class="server-config-body">
          <div class="grid server-grid">
            <label>
              <span>{{ t('名称') }}</span>
              <input v-model="server.name" :placeholder="t('例如：主服务器')" />
            </label>
            <label>
              <span>{{ t('Emby 地址') }}</span>
              <input
                v-model="server.emby_host"
                placeholder="http://emby.local:8096"
              />
            </label>
            <label>
              <span>Emby API Key</span>
              <div class="secret-input">
                <input
                  v-model="server.emby_api_key"
                  :type="isApiKeyVisible(server.id) ? 'text' : 'password'"
                  autocomplete="off"
                />
                <button
                  type="button"
                  class="secret-toggle"
                  :aria-pressed="isApiKeyVisible(server.id)"
                  :aria-label="
                    isApiKeyVisible(server.id)
                      ? t('隐藏 Emby API Key')
                      : t('显示 Emby API Key')
                  "
                  :title="
                    isApiKeyVisible(server.id)
                      ? t('隐藏 Emby API Key')
                      : t('显示 Emby API Key')
                  "
                  @click="toggleApiKeyVisible(server.id)"
                >
                  <EyeOff
                    v-if="!isApiKeyVisible(server.id)"
                    :size="16"
                    aria-hidden="true"
                  />
                  <Eye v-else :size="16" aria-hidden="true" />
                </button>
              </div>
            </label>
            <label>
              <span>{{ t('反代端口') }}</span>
              <input
                v-model.number="server.port"
                type="number"
                min="1"
                max="65535"
              />
            </label>
          </div>
          <div class="grid real-ip-grid">
            <label>
              <span>{{ t('真实 IP 获取方式') }}</span>
              <select
                v-model="server.real_ip_mode"
                @change="updateRealIpMode(server)"
              >
                <option
                  v-for="option in realIpModeOptions"
                  :key="option.value"
                  :value="option.value"
                >
                  {{ t(option.label) }}
                </option>
              </select>
              <small
                v-if="server.real_ip_mode === 'header_list'"
                class="field-help"
              >
                {{
                  t(
                    '从下列常用 CDN 携带真实 IP 的 HTTP Header 中获取，按顺序取第一个能获取到的值。',
                  )
                }}
              </small>
            </label>
            <label v-if="needsRealIpHeader(server)">
              <span>{{
                server.real_ip_mode === 'header' ? 'HTTP Header' : 'CDN Headers'
              }}</span>
              <textarea
                v-model="server.real_ip_header"
                :placeholder="
                  server.real_ip_mode === 'header'
                    ? t('例如：x-real-ip')
                    : defaultCdnHeaders
                "
              />
            </label>
          </div>
          <p class="muted real-ip-help">
            {{
              t(
                '默认使用系统识别。经过 CDN 或多层反代后 IP 不准时再配置，保存后会同步重启对应反代服务。',
              )
            }}
          </p>
          <div class="server-config-actions">
            <button class="primary" type="submit" :disabled="saving">
              <Check :size="15" />{{ saving ? t('保存中') : t('保存配置') }}
            </button>
          </div>
        </div>
      </article>
    </div>

    <div class="config-section-head">
      <h3>{{ t('缓存与巡检') }}</h3>
    </div>
    <div class="grid common-grid">
      <label>
        <span>{{ t('缓存秒数') }}</span>
        <input
          class="compact-number-input"
          v-model.number="settings.cache_ttl_seconds"
          type="number"
          min="0"
        />
      </label>
      <label>
        <span>{{ t('缓存最大条数') }}</span>
        <input
          v-model.number="settings.cache_max_capacity"
          type="number"
          min="1"
        />
      </label>
      <label>
        <span>{{ t('OpenList 地址') }}</span>
        <input
          v-model="settings.openlist_addr"
          :placeholder="`${t('可选')}：http://openlist.local:5244`"
        />
      </label>
      <label>
        <span>OpenList Token</span>
        <input
          v-model="settings.openlist_token"
          type="password"
          :placeholder="t('可选')"
        />
      </label>
    </div>
    <div class="grid health-check-grid">
      <label class="check setting-check">
        <input v-model="settings.connectivity_check_enabled" type="checkbox" />
        <span>{{ t('启用服务器连通性巡检') }}</span>
      </label>
      <label>
        <span>{{ t('巡检间隔秒数') }}</span>
        <input
          class="compact-number-input"
          v-model.number="settings.connectivity_check_interval_seconds"
          type="number"
          min="10"
          max="3600"
        />
      </label>
      <label>
        <span>{{ t('单项超时秒数') }}</span>
        <input
          class="compact-number-input"
          v-model.number="settings.connectivity_check_timeout_seconds"
          type="number"
          min="1"
          max="60"
        />
      </label>
      <label>
        <span>{{ t('反代无响应自动重启秒数') }}</span>
        <input
          class="compact-number-input"
          v-model.number="settings.connectivity_auto_restart_seconds"
          type="number"
          min="0"
          max="86400"
        />
        <small class="field-help">{{
          t('填 0 表示不自动重启；只在反代端口连续无响应时触发。')
        }}</small>
      </label>
    </div>
    <div class="config-section-head">
      <h3>{{ t('直链与重定向') }}</h3>
    </div>
    <div class="advanced-routing-grid">
      <label class="cache-filter-mode">
        <span>{{ t('缓存过滤模式') }}</span>
        <select v-model="settings.cache_domain_filter_mode">
          <option value="off">{{ t('不过滤') }}</option>
          <option value="whitelist">{{ t('白名单：命中才缓存') }}</option>
          <option value="blacklist">{{ t('黑名单：命中不缓存') }}</option>
        </select>
      </label>
      <label class="cache-filter-domains">
        <span>{{ t('缓存过滤域名') }}</span>
        <textarea
          v-model="settings.cache_domain_whitelist"
          :disabled="settings.cache_domain_filter_mode === 'off'"
          :placeholder="
            t('支持多个域名、通配符或关键字；每行一个，例如：*.115cdn.* 或 115')
          "
        />
        <small class="field-help">
          {{
            t(
              '只匹配直链域名部分。白名单命中才缓存；黑名单命中不缓存，其他直链正常缓存。',
            )
          }}
        </small>
      </label>
      <div class="head-resolve-grid">
        <label class="check">
          <input v-model="settings.enable_internal_redirect" type="checkbox" />
          <span>{{ t('开启内部重定向 HEAD 解析') }}</span>
        </label>
        <label>
          <span>{{ t('HEAD 超时秒数') }}</span>
          <input
            class="compact-number-input"
            v-model.number="settings.internal_redirect_timeout_seconds"
            type="number"
            min="1"
          />
        </label>
      </div>
      <label class="strm-mapping-field">
        <span>{{ t('STRM URL 映射') }}</span>
        <textarea
          class="strm-mapping-input"
          v-model="settings.strm_url_mappings"
          spellcheck="false"
          :placeholder="strmMappingPlaceholder"
        />
      </label>
    </div>

    <section class="config-tools single">
      <div class="tool-block">
        <div class="panel-head compact">
          <h3>{{ t('配置测试结果') }}</h3>
          <button
            class="secondary"
            type="button"
            :disabled="saving"
            @click="validateSettings"
          >
            <RefreshCw :size="15" />{{ t('重新测试') }}
          </button>
        </div>
        <div v-if="validationResults.length" class="validation-list">
          <div
            v-for="result in validationResults"
            :key="`${result.scope}-${result.message}-${result.detail}`"
            :class="['validation-row', validationClass(result)]"
          >
            <strong>{{ localizeValidationText(result.scope) }}</strong>
            <span>{{ localizeValidationText(result.message) }}</span>
            <small>{{
              result.detail ? localizeValidationText(result.detail) : '--'
            }}</small>
          </div>
        </div>
        <div v-else class="empty-state compact">
          {{ t('还没有运行配置测试。') }}
        </div>
      </div>
    </section>
  </form>
</template>
