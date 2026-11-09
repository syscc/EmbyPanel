<script setup lang="ts">
import { Bell, History, Plus, Trash2, Webhook } from '@lucide/vue'
import { computed, onBeforeUnmount, onMounted } from 'vue'
import CheckboxField from '@/components/ui/CheckboxField.vue'
import RefreshButton from '@/components/ui/RefreshButton.vue'
import SettingsDialogShell from '@/components/ui/SettingsDialogShell.vue'
import UiSwitch from '@/components/ui/UiSwitch.vue'
import { usePanelContext } from '@/composables/panel-context'
import {
  useAlertsController,
  type AlertDelivery,
} from '@/composables/useAlertsController'

const { t, api, showNotice, confirmAction, promptAction, formatTimestampMs } =
  usePanelContext()

const controller = useAlertsController({
  api,
  translate: t,
  showNotice,
  confirmAction,
  promptAction,
})

const {
  rules,
  deliveries,
  historyByRule,
  loading,
  deliveriesLoading,
  saving,
  error,
  deliveriesError,
  historyError,
  actionError,
  dialogOpen,
  draft,
  historyRuleId,
  historyOpen,
  historyLoading,
  togglingIds,
  testingIds,
  silencingIds,
  deletingIds,
  clearRuleConfig,
  loadRules,
  loadDeliveries,
  openRuleHistory,
  openCreateDialog,
  openEditDialog,
  setDialogOpen,
  setHistoryOpen,
  saveDraft,
  toggleRule,
  deleteRule,
  testRule,
  promptSilence,
  clearSilence,
  refreshAll,
  dispose,
} = controller

const isEditing = computed(() => Boolean(draft.value.id))
const dialogTitle = computed(() =>
  isEditing.value ? t('编辑告警规则') : t('新建告警规则'),
)
const activeHistory = computed<AlertDelivery[]>(() => {
  const id = historyRuleId.value
  if (!id) return []
  return historyByRule.value[id] ?? []
})
const activeHistoryRuleName = computed(() => {
  const id = historyRuleId.value
  if (!id) return ''
  return rules.value.find((rule) => rule.id === id)?.name || id
})

onMounted(() => {
  void refreshAll()
})

onBeforeUnmount(() => {
  dispose()
})

function deliveryStatusLabel(status: string) {
  if (status === 'success') return t('成功')
  if (status === 'failed') return t('失败')
  return status
}

function handleDialogOpenChange(open: boolean) {
  if (saving.value && !open) return
  setDialogOpen(open)
}
</script>

<template>
  <section class="client-page alerts-page">
    <div class="panel">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <Bell :size="18" />
            <h2>{{ t('告警规则') }}</h2>
          </div>
          <p class="muted">
            {{
              t(
                '配置 Webhook 告警规则、静默与测试发送。Webhook URL 由管理员自行配置。',
              )
            }}
          </p>
        </div>
        <div class="panel-actions">
          <RefreshButton
            :label="t('刷新')"
            :loading-label="t('刷新中')"
            :action="refreshAll"
            :loading="loading"
            :disabled="loading"
          />
          <button class="primary" type="button" @click="openCreateDialog">
            <Plus :size="15" />
            {{ t('添加规则') }}
          </button>
        </div>
      </div>

      <div v-if="error" class="notice error" role="alert">{{ error }}</div>
      <div v-if="actionError" class="notice error" role="alert">
        {{ actionError }}
      </div>

      <div v-if="loading && rules.length === 0" class="empty-state">
        {{ t('加载中') }}
      </div>
      <div v-else-if="!loading && rules.length === 0" class="empty-state">
        {{ t('暂无告警规则') }}
      </div>

      <div v-else class="alert-rule-list">
        <article v-for="rule in rules" :key="rule.id" class="alert-rule-card">
          <div class="alert-rule-head">
            <div>
              <h3>{{ rule.name }}</h3>
              <p class="muted mono">{{ rule.id }}</p>
            </div>
            <UiSwitch
              :model-value="rule.enabled"
              :label="t('启用')"
              :disabled="Boolean(togglingIds[rule.id])"
              size="compact"
              @update:model-value="(value) => toggleRule(rule, value)"
            />
          </div>

          <div class="alert-rule-meta">
            <div>
              <span class="meta-label">{{ t('阈值') }}</span>
              <span>{{ rule.threshold || '--' }}</span>
            </div>
            <div>
              <span class="meta-label">{{ t('静默周期（秒）') }}</span>
              <span>{{ rule.silence_period }}</span>
            </div>
            <div>
              <span class="meta-label">{{ t('创建时间') }}</span>
              <span>{{ formatTimestampMs(Number(rule.created_at)) }}</span>
            </div>
          </div>

          <p class="muted webhook-line">
            <span class="meta-label">{{ t('Webhook 配置') }}</span>
            {{ rule.rule_configured ? t('已配置') : t('未配置') }}
          </p>

          <div class="rule-actions">
            <button
              class="secondary"
              type="button"
              @click="openEditDialog(rule)"
            >
              {{ t('编辑') }}
            </button>
            <button
              class="secondary"
              type="button"
              :disabled="Boolean(testingIds[rule.id])"
              @click="testRule(rule)"
            >
              <Webhook :size="15" />
              {{ testingIds[rule.id] ? t('测试中') : t('测试 Webhook') }}
            </button>
            <button
              class="secondary"
              type="button"
              :disabled="Boolean(silencingIds[rule.id])"
              @click="promptSilence(rule)"
            >
              {{ t('设置静默') }}
            </button>
            <button
              class="secondary"
              type="button"
              :disabled="Boolean(silencingIds[rule.id])"
              @click="clearSilence(rule)"
            >
              {{ t('取消静默') }}
            </button>
            <button
              class="secondary"
              type="button"
              :disabled="historyLoading && historyRuleId === rule.id"
              @click="openRuleHistory(rule.id)"
            >
              <History :size="15" />
              {{ t('查看历史') }}
            </button>
            <button
              class="danger-button"
              type="button"
              :disabled="Boolean(deletingIds[rule.id])"
              @click="deleteRule(rule)"
            >
              <Trash2 :size="15" />
              {{ t('删除') }}
            </button>
          </div>
        </article>
      </div>
    </div>

    <div class="panel">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <History :size="18" />
            <h2>{{ t('告警交付') }}</h2>
          </div>
          <p class="muted">{{ t('最近交付') }}</p>
        </div>
        <div class="panel-actions">
          <RefreshButton
            :label="t('刷新')"
            :loading-label="t('刷新中')"
            :action="() => loadDeliveries()"
            :loading="deliveriesLoading"
            :disabled="deliveriesLoading"
          />
        </div>
      </div>

      <div v-if="deliveriesError" class="notice error" role="alert">
        {{ deliveriesError }}
      </div>
      <div
        v-if="deliveriesLoading && deliveries.length === 0"
        class="empty-state"
      >
        {{ t('加载中') }}
      </div>
      <div
        v-else-if="!deliveriesLoading && deliveries.length === 0"
        class="empty-state"
      >
        {{ t('暂无交付记录') }}
      </div>
      <div v-else class="delivery-table-wrap">
        <table class="delivery-table">
          <thead>
            <tr>
              <th>{{ t('时间') }}</th>
              <th>{{ t('规则') }}</th>
              <th>{{ t('状态') }}</th>
              <th>{{ t('重试次数') }}</th>
              <th>{{ t('消息') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="row in deliveries" :key="row.id">
              <td>{{ formatTimestampMs(Number(row.timestamp_ms)) }}</td>
              <td class="mono">{{ row.rule_id }}</td>
              <td>
                <span
                  class="delivery-status"
                  :class="{
                    ok: row.status === 'success',
                    bad: row.status === 'failed',
                  }"
                >
                  {{ deliveryStatusLabel(row.status) }}
                </span>
              </td>
              <td>{{ row.retry_count }}</td>
              <td>{{ row.message }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <SettingsDialogShell
      :open="dialogOpen"
      :title="dialogTitle"
      :description="t('配置告警规则')"
      :close-label="t('关闭')"
      content-class="alert-rule-dialog"
      show-description
      @update:open="handleDialogOpenChange"
    >
      <template #icon>
        <Bell :size="19" />
      </template>

      <form class="settings-dialog-form" @submit.prevent="saveDraft">
        <div v-if="actionError" class="notice error" role="alert">
          {{ actionError }}
        </div>
        <label>
          <span>{{ t('规则名称') }}</span>
          <input
            v-model="draft.name"
            :placeholder="t('规则名称')"
            :disabled="saving"
            maxlength="128"
            required
          />
        </label>
        <CheckboxField
          v-model="draft.enabled"
          :label="t('启用')"
          :disabled="saving"
        />
        <label>
          <span>{{ t('Webhook 配置') }}</span>
          <textarea
            v-model="draft.rule_config"
            rows="3"
            maxlength="4096"
            :placeholder="
              isEditing && !clearRuleConfig
                ? t('保持现有配置')
                : t('Webhook URL 或 JSON 配置')
            "
            :disabled="saving || (isEditing && clearRuleConfig)"
            @input="clearRuleConfig = false"
          />
        </label>
        <div v-if="isEditing">
          <CheckboxField
            v-model="clearRuleConfig"
            :label="t('清空 webhook 配置')"
            :disabled="saving"
          />
        </div>
        <div class="grid two">
          <label for="alert-rule-metric">
            <span>{{ t('指标') }}</span>
            <select
              id="alert-rule-metric"
              v-model="draft.metric"
              :disabled="saving"
            >
              <option value="">{{ t('仅手动/测试') }}</option>
              <option value="proxy.requests">{{ t('proxy.requests') }}</option>
              <option value="proxy.redirects">
                {{ t('proxy.redirects') }}
              </option>
              <option value="proxy.cache_hits">
                {{ t('proxy.cache_hits') }}
              </option>
              <option value="proxy.blocks">{{ t('proxy.blocks') }}</option>
              <option value="proxy.errors">{{ t('proxy.errors') }}</option>
            </select>
          </label>
          <label for="alert-rule-operator">
            <span>{{ t('比较条件') }}</span>
            <select
              id="alert-rule-operator"
              v-model="draft.operator"
              :disabled="saving"
            >
              <option value="gt">&gt;</option>
              <option value="gte">≥</option>
              <option value="lt">&lt;</option>
              <option value="lte">≤</option>
              <option value="eq">=</option>
            </select>
          </label>
          <label for="alert-rule-threshold">
            <span>{{ t('阈值') }}</span>
            <input
              id="alert-rule-threshold"
              v-model="draft.threshold"
              :type="draft.metric ? 'number' : 'text'"
              :inputmode="draft.metric ? 'decimal' : 'text'"
              :step="draft.metric ? 'any' : undefined"
              :placeholder="t('阈值')"
              :disabled="saving"
              :required="Boolean(draft.metric)"
            />
          </label>
          <label for="alert-rule-server-id">
            <span>{{ t('服务器 ID') }}</span>
            <input
              id="alert-rule-server-id"
              v-model="draft.server_id"
              maxlength="128"
              :placeholder="t('全部服务器')"
              :disabled="saving"
            />
          </label>
          <label for="alert-rule-evaluation-window">
            <span>{{ t('评估窗口') }} ({{ t('秒') }})</span>
            <input
              id="alert-rule-evaluation-window"
              v-model.number="draft.evaluation_window_secs"
              type="number"
              min="0"
              max="604800"
              step="1"
              :disabled="saving"
              required
            />
          </label>
          <label for="alert-rule-silence-period">
            <span>{{ t('静默周期（秒）') }}</span>
            <input
              id="alert-rule-silence-period"
              v-model.number="draft.silence_period"
              type="number"
              min="0"
              step="1"
              :disabled="saving"
            />
          </label>
        </div>
      </form>

      <template #footer>
        <button
          class="secondary"
          type="button"
          :disabled="saving"
          @click="setDialogOpen(false)"
        >
          {{ t('取消') }}
        </button>
        <button
          class="primary"
          type="button"
          :disabled="saving"
          @click="saveDraft"
        >
          {{ saving ? t('保存中') : t('保存') }}
        </button>
      </template>
    </SettingsDialogShell>

    <SettingsDialogShell
      :open="historyOpen"
      :title="t('交付历史')"
      :description="activeHistoryRuleName"
      :close-label="t('关闭')"
      content-class="alert-history-dialog"
      show-description
      @update:open="setHistoryOpen"
    >
      <template #icon>
        <History :size="19" />
      </template>

      <div v-if="historyError" class="notice error" role="alert">
        {{ historyError }}
      </div>
      <div v-if="historyLoading" class="empty-state">{{ t('加载中') }}</div>
      <div v-else-if="activeHistory.length === 0" class="empty-state">
        {{ t('暂无交付记录') }}
      </div>
      <div v-else class="history-list">
        <article
          v-for="item in activeHistory"
          :key="item.id"
          class="history-item"
        >
          <div class="history-item-head">
            <span
              class="delivery-status"
              :class="{
                ok: item.status === 'success',
                bad: item.status === 'failed',
              }"
            >
              {{ deliveryStatusLabel(item.status) }}
            </span>
            <span class="muted">{{
              formatTimestampMs(Number(item.timestamp_ms))
            }}</span>
          </div>
          <p>{{ item.message }}</p>
          <p class="muted">{{ t('重试次数') }}: {{ item.retry_count }}</p>
        </article>
      </div>

      <template #footer>
        <button class="secondary" type="button" @click="setHistoryOpen(false)">
          {{ t('关闭') }}
        </button>
      </template>
    </SettingsDialogShell>
  </section>
</template>

<style scoped>
.alerts-page {
  display: grid;
  gap: 1rem;
}

.alert-rule-list {
  display: grid;
  gap: 0.85rem;
}

.alert-rule-card {
  border: 1px solid var(--border);
  border-radius: 12px;
  padding: 1rem;
  background: color-mix(in srgb, var(--card, #111) 92%, transparent);
}

.alert-rule-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
}

.alert-rule-head h3 {
  margin: 0 0 0.25rem;
  font-size: var(--font-size-section);
  font-weight: 700;
}

.alert-rule-meta {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 0.75rem;
  margin: 0.9rem 0;
  font-size: var(--font-size-body);
}

.meta-label {
  display: block;
  color: var(--muted, #8b93a7);
  font-size: var(--font-size-label);
  font-weight: 700;
  margin-bottom: 0.15rem;
}

.webhook-line {
  margin: 0 0 0.9rem;
  word-break: break-all;
}

.rule-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}

.delivery-table-wrap {
  overflow-x: auto;
}

.delivery-table {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--font-size-body);
}

.delivery-table th {
  font-size: var(--font-size-label);
  font-weight: 700;
}

.delivery-table th,
.delivery-table td {
  text-align: left;
  padding: 0.55rem 0.4rem;
  border-bottom: 1px solid var(--border);
  vertical-align: top;
}

.delivery-status.ok {
  color: var(--success, #3dd68c);
}

.delivery-status.bad {
  color: var(--danger, #ff6b6b);
}

.history-list {
  display: grid;
  gap: 0.75rem;
}

.history-item {
  font-size: var(--font-size-body);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 0.75rem 0.9rem;
}

.history-item-head {
  display: flex;
  justify-content: space-between;
  gap: 0.75rem;
  margin-bottom: 0.35rem;
}

.history-item .muted {
  font-size: var(--font-size-meta);
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12px;
}

.grid.two {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0.75rem;
}

@media (max-width: 860px) {
  .alert-rule-meta {
    grid-template-columns: 1fr;
  }
  .grid.two {
    grid-template-columns: 1fr;
  }
}
</style>
