<script setup lang="ts">
import { Check, FileText, RefreshCw, UserRound } from '@lucide/vue'
import { usePanelContext } from '@/composables/panel-context'

const {
  profile,
  profileForm,
  savingAccount,
  passwordForm,
  auditLogs,
  auditLogsError,
  auditKeywordFilter,
  selectedAuditAction,
  auditActionOptions,
  t,
  saveAccount,
  refreshAuditLogs,
  formatTimestampMs,
} = usePanelContext()
</script>

<template>
  <section class="account-grid">
    <section class="panel account-settings-panel">
      <form class="account-form" @submit.prevent="saveAccount">
        <div class="panel-head">
          <div>
            <div class="panel-title-line">
              <UserRound :size="18" />
              <h2>{{ t('账户与安全') }}</h2>
            </div>
            <p id="account-help" class="muted">
              {{ t('用户名留空保持不变；密码字段全部留空时不修改密码。') }}
            </p>
          </div>
        </div>

        <div class="account-controls-row">
          <div class="account-fields-grid">
            <label>
              <span>{{ t('用户名') }}</span>
              <input
                v-model="profileForm.username"
                name="username"
                autocomplete="username"
                :placeholder="profile.username"
                aria-describedby="account-help"
                :disabled="savingAccount"
              />
            </label>
            <label>
              <span>{{ t('当前密码') }}</span>
              <input
                v-model="passwordForm.current_password"
                name="current-password"
                type="password"
                autocomplete="current-password"
                aria-describedby="account-help"
                :disabled="savingAccount"
              />
            </label>
            <label>
              <span>{{ t('新密码') }}</span>
              <input
                v-model="passwordForm.new_password"
                name="new-password"
                type="password"
                autocomplete="new-password"
                aria-describedby="account-help"
                :disabled="savingAccount"
              />
            </label>
            <label>
              <span>{{ t('确认新密码') }}</span>
              <input
                v-model="passwordForm.confirm_password"
                name="confirm-password"
                type="password"
                autocomplete="new-password"
                aria-describedby="account-help"
                :disabled="savingAccount"
              />
            </label>
          </div>

          <div class="form-actions">
            <button class="primary" type="submit" :disabled="savingAccount">
              <Check :size="15" />{{
                savingAccount ? t('保存中') : t('保存账户设置')
              }}
            </button>
          </div>
        </div>
      </form>
    </section>

    <section class="panel audit-panel">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <FileText :size="18" />
            <h2>{{ t('配置审计') }}</h2>
          </div>
          <p class="muted">
            {{
              t('记录配置、账户、通知、备份恢复等管理操作，不保存敏感明文。')
            }}
          </p>
        </div>
        <button class="secondary" @click="refreshAuditLogs">
          <RefreshCw :size="15" />{{ t('刷新') }}
        </button>
      </div>
      <div class="audit-toolbar">
        <label>
          <span>{{ t('操作类型') }}</span>
          <select v-model="selectedAuditAction" @change="refreshAuditLogs">
            <option
              v-for="action in auditActionOptions"
              :key="action"
              :value="action"
            >
              {{ action === 'all' ? t('全部操作') : action }}
            </option>
          </select>
        </label>
        <label>
          <span>{{ t('关键词') }}</span>
          <input
            v-model="auditKeywordFilter"
            :placeholder="t('搜索管理员 / 操作 / 摘要')"
            @keyup.enter="refreshAuditLogs"
          />
        </label>
      </div>
      <div v-if="auditLogsError" class="notice error" role="alert">
        {{ t('审计记录加载失败') }}: {{ auditLogsError }}
      </div>
      <div v-else-if="auditLogs.length" class="audit-list">
        <article v-for="entry in auditLogs" :key="entry.id" class="audit-row">
          <div>
            <strong>{{ entry.action }}</strong>
            <span>{{ entry.summary }}</span>
          </div>
          <small
            >{{ entry.admin_username || '--' }} · {{ entry.result }} ·
            {{ formatTimestampMs(entry.timestamp_ms) }}</small
          >
        </article>
      </div>
      <div v-else class="empty-state compact">{{ t('暂无审计记录。') }}</div>
    </section>
  </section>
</template>

<style scoped>
.account-grid {
  grid-template-columns: minmax(0, 1fr);
}

.account-settings-panel,
.audit-panel {
  grid-column: 1 / -1;
}

.account-form {
  display: grid;
  gap: 18px;
  width: 100%;
}

.account-form .panel-head {
  margin-bottom: 0;
}

.account-controls-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: end;
  gap: 16px;
  min-width: 0;
}

.account-fields-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
  min-width: 0;
}

.account-controls-row > .form-actions {
  margin: 0;
}

@media (max-width: 1199px) {
  .account-fields-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

@media (max-width: 700px) {
  .account-controls-row {
    grid-template-columns: 1fr;
  }

  .account-fields-grid {
    grid-template-columns: 1fr;
  }
}
</style>
