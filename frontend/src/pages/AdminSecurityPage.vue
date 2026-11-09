<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { usePanelContext } from '@/composables/panel-context'
import { useAdminSecurityController, type Admin } from '@/composables/useAdminSecurityController'
import RefreshButton from '@/components/ui/RefreshButton.vue'
import SettingsDialogShell from '@/components/ui/SettingsDialogShell.vue'
import UiSwitch from '@/components/ui/UiSwitch.vue'
import CheckboxField from '@/components/ui/CheckboxField.vue'
import { useActionDialog } from '@/composables/useActionDialog'
import { formatTimestamp } from '@/lib/panel-formatters'

const panel = usePanelContext()
const controller = useAdminSecurityController({
  api: panel.api,
  encryptPayload: panel.encryptPayload,
})
const { t, showNotice } = panel
const { confirmAction, promptAction } = useActionDialog()
const {
  admins,
  roles,
  tokens,
  sessions,
  currentUserId,
  twoFactor,
  createdToken,
  pendingTwoFactor,
  recoveryCodes,
  loading,
  operationPending,
  error,
  dto,
} = controller

const showAdminCreateDialog = ref(false)
const showRoleDialog = ref(false)
const showGrantDialog = ref(false)
const showTokenCreateDialog = ref(false)
const showTokenResultDialog = ref(false)
const showTwoFactorDialog = ref(false)
const showRecoveryDialog = ref(false)
const refreshing = ref(false)
const selectedAdmin = ref<Admin | null>(null)
const roleDraft = ref<string[]>([])
const serverGrantDraft = ref<string[]>([])
const adminCreate = reactive({
  username: '',
  password: '',
  displayName: '',
  roleKeys: [] as string[],
  serverIds: [] as string[],
})

const currentAdmin = computed(() => admins.value.find((admin) => admin.id === currentUserId.value) ?? null)
const canManageAdmins = computed(() => currentAdmin.value?.role_keys.includes('super_admin') ?? false)
const canWriteSecurity = computed(() => {
  const admin = currentAdmin.value
  return Boolean(admin?.role_keys.includes('super_admin') || admin?.permission_keys.includes('admin.security.write'))
})
const serverOptions = computed(() => panel.serverOptions.value.map((server) => ({ id: server.id, name: server.name || server.id })))
let refreshGeneration = 0

onMounted(() => {
  void controller.load()
})

async function refreshPage() {
  if (refreshing.value || loading.value) return
  const generation = ++refreshGeneration
  refreshing.value = true
  try {
    if (!await panel.refreshServerOptions()) return
    await controller.load()
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    if (generation === refreshGeneration) refreshing.value = false
  }
}

function timestamp(value: number | string | null | undefined) {
  return formatTimestamp(String(value ?? ''), panel.locale.value)
}

function updateSelection(values: string[], value: string, selected: boolean) {
  if (selected) return values.includes(value) ? values : [...values, value]
  return values.filter((item) => item !== value)
}

function openAdminCreateDialog() {
  if (controller.operationPending.value || !canManageAdmins.value) return
  adminCreate.username = ''
  adminCreate.password = ''
  adminCreate.displayName = ''
  adminCreate.roleKeys = []
  adminCreate.serverIds = []
  showAdminCreateDialog.value = true
}

async function createAdmin() {
  if (!canManageAdmins.value) return
  try {
    await controller.createAdmin(adminCreate)
    adminCreate.password = ''
    showAdminCreateDialog.value = false
    showNotice(t('管理员已创建'))
  } catch {
    // The controller owns the safe error text.
  }
}

function openRoleEditor(admin: Admin) {
  if (controller.operationPending.value || !canManageAdmins.value) return
  selectedAdmin.value = admin
  roleDraft.value = [...admin.role_keys]
  showRoleDialog.value = true
}

async function saveRoles() {
  if (!canManageAdmins.value || !selectedAdmin.value) return
  try {
    await controller.updateAdminRoles(selectedAdmin.value.id, roleDraft.value)
    showRoleDialog.value = false
    selectedAdmin.value = null
    showNotice(t('角色已保存'))
  } catch {
    // The controller owns the safe error text.
  }
}

function openGrantEditor(admin: Admin) {
  if (controller.operationPending.value || !canManageAdmins.value) return
  selectedAdmin.value = admin
  serverGrantDraft.value = admin.server_grants.map((grant) => grant.server_id)
  showGrantDialog.value = true
}

async function saveServerGrants() {
  if (!canManageAdmins.value || !selectedAdmin.value) return
  try {
    await controller.updateAdminServerGrants(selectedAdmin.value.id, serverGrantDraft.value)
    showGrantDialog.value = false
    selectedAdmin.value = null
    showNotice(t('服务器授权已保存'))
  } catch {
    // The controller owns the safe error text.
  }
}

async function setAdminEnabled(admin: Admin, enabled: boolean) {
  if (!canManageAdmins.value) return
  if (admin.id === currentUserId.value && !enabled) return
  if (!enabled) {
    const confirmed = await confirmAction({
      title: t('禁用管理员'),
      description: t('禁用会撤销该管理员的全部会话和 API Token，确定继续吗？'),
      confirmText: t('禁用'),
      cancelText: t('取消'),
      tone: 'danger',
    })
    if (!confirmed || !canManageAdmins.value) return
  }
  try {
    await controller.setAdminEnabled(admin.id, enabled)
    showNotice(enabled ? t('管理员已启用') : t('管理员已禁用'))
  } catch {
    // The controller owns the safe error text.
  }
}

async function resetAdminPassword(admin: Admin) {
  if (!canManageAdmins.value) return
  const password = await promptAction({
    title: t('重置管理员密码'),
    description: t('重置密码会撤销该管理员的全部会话和 API Token'),
    inputLabel: t('新密码'),
    inputType: 'password',
    inputAutocomplete: 'new-password',
    minLength: 4,
  })
  if (password === null || !canManageAdmins.value) return
  try {
    await controller.resetAdminPassword(admin.id, password)
    showNotice(t('管理员密码已重置'))
  } catch {
    // The controller owns the safe error text.
  }
}

async function deleteAdmin(admin: Admin) {
  if (!canManageAdmins.value) return
  const confirmed = await confirmAction({
    title: t('删除管理员'),
    description: t('删除管理员会同时撤销其会话和 Token，且不可恢复。'),
    confirmText: t('删除'),
    cancelText: t('取消'),
    tone: 'danger',
  })
  if (!confirmed || !canManageAdmins.value) return
  try {
    await controller.deleteAdmin(admin.id)
    showNotice(t('管理员已删除'))
  } catch {
    // The controller owns the safe error text.
  }
}

async function openTokenCreateDialog() {
  if (controller.operationPending.value || !canWriteSecurity.value) return
  showTokenCreateDialog.value = true
}

async function createToken() {
  if (!canWriteSecurity.value) return
  try {
    await controller.createToken()
    showTokenCreateDialog.value = false
    showTokenResultDialog.value = true
    showNotice(t('Token 创建成功'))
  } catch {
    // The controller owns the safe error text.
  }
}

function closeTokenResult(open: boolean) {
  showTokenResultDialog.value = open
  if (!open) controller.clearCreatedToken()
}

async function revokeToken(id: number) {
  if (!canWriteSecurity.value) return
  const confirmed = await confirmAction({
    title: t('撤销 Token'),
    description: t('撤销后该 Token 将立即失效，确定继续吗？'),
    confirmText: t('撤销'),
    cancelText: t('取消'),
    tone: 'danger',
  })
  if (!confirmed || !canWriteSecurity.value) return
  try {
    await controller.revokeToken(id)
    showNotice(t('操作成功'))
  } catch {
    // The controller owns the safe error text.
  }
}

async function revokeSession(id: number) {
  if (!canWriteSecurity.value) return
  const confirmed = await confirmAction({
    title: t('撤销会话'),
    description: t('撤销后该登录会话将立即失效，确定继续吗？'),
    confirmText: t('撤销'),
    cancelText: t('取消'),
    tone: 'danger',
  })
  if (!confirmed || !canWriteSecurity.value) return
  try {
    await controller.revokeSession(id)
    showNotice(t('操作成功'))
  } catch {
    // The controller owns the safe error text.
  }
}

async function startTwoFactor() {
  if (!canWriteSecurity.value) return
  const status = controller.twoFactor.value
  if (!status.available || !status.login_challenge_available) {
    showNotice(t('当前登录界面尚未支持 2FA 验证，暂不能启用'))
    return
  }
  try {
    await controller.setup2fa()
    showTwoFactorDialog.value = true
  } catch {
    // The controller owns the safe error text.
  }
}

async function confirmTwoFactor() {
  if (!canWriteSecurity.value) return
  const code = await promptAction({
    title: t('确认 2FA'),
    description: t('请输入验证器当前显示的 6 位验证码'),
    inputLabel: t('验证码'),
    inputType: 'text',
    inputAutocomplete: 'off',
    minLength: 6,
  })
  if (code === null || !canWriteSecurity.value) return
  try {
    await controller.confirm2fa(code)
    showTwoFactorDialog.value = false
    showRecoveryDialog.value = true
    showNotice(t('2FA 已启用，请保存恢复码'))
  } catch {
    // The controller owns the safe error text.
  }
}

async function disableTwoFactor() {
  if (!canWriteSecurity.value) return
  const confirmed = await confirmAction({
    title: t('禁用 2FA'),
    description: t('禁用会撤销当前账号全部会话和 API Token，确定继续吗？'),
    confirmText: t('禁用'),
    cancelText: t('取消'),
    tone: 'danger',
  })
  if (!confirmed || !canWriteSecurity.value) return
  const password = await promptAction({
    title: t('验证管理员密码'),
    inputLabel: t('当前密码'),
    inputType: 'password',
    inputAutocomplete: 'current-password',
    minLength: 1,
  })
  if (password === null || !canWriteSecurity.value) return
  const factor = await promptAction({
    title: t('验证 2FA'),
    description: t('请输入 2FA 验证码或恢复码'),
    inputLabel: t('验证码或恢复码'),
    inputType: 'text',
    inputAutocomplete: 'off',
    minLength: 1,
  })
  if (factor === null || !canWriteSecurity.value) return
  try {
    await controller.disable2fa(password, factor)
    showNotice(t('2FA 已禁用，请重新登录'))
  } catch {
    // The controller owns the safe error text.
  }
}

async function regenerateRecoveryCodes() {
  if (!canWriteSecurity.value) return
  const password = await promptAction({
    title: t('验证管理员密码'),
    inputLabel: t('当前密码'),
    inputType: 'password',
    inputAutocomplete: 'current-password',
    minLength: 1,
  })
  if (password === null || !canWriteSecurity.value) return
  const factor = await promptAction({
    title: t('验证 2FA'),
    description: t('请输入 2FA 验证码或恢复码'),
    inputLabel: t('验证码或恢复码'),
    inputType: 'text',
    inputAutocomplete: 'off',
    minLength: 1,
  })
  if (factor === null || !canWriteSecurity.value) return
  try {
    await controller.regenerateRecoveryCodes(password, factor)
    showRecoveryDialog.value = true
    showNotice(t('恢复码已重新生成'))
  } catch {
    // The controller owns the safe error text.
  }
}

async function rollbackTwoFactor() {
  if (!canWriteSecurity.value) return
  const confirmed = await confirmAction({
    title: t('回滚 2FA 设置'),
    description: t('将删除尚未确认的 2FA 设置，确定继续吗？'),
    confirmText: t('回滚'),
    cancelText: t('取消'),
    tone: 'danger',
  })
  if (!confirmed || !canWriteSecurity.value) return
  try {
    await controller.rollback2fa()
    showNotice(t('操作成功'))
  } catch {
    // The controller owns the safe error text.
  }
}

function closeRecoveryDialog(open: boolean) {
  showRecoveryDialog.value = open
  if (!open) controller.clearRecoveryCodes()
}
</script>

<template>
  <div class="admin-security-page">
    <header class="page-head">
      <div>
        <h1>{{ t('管理员安全') }}</h1>
        <p>{{ t('多管理员安全管理') }}</p>
      </div>
      <RefreshButton
        :label="t('刷新')"
        :loading-label="t('刷新中')"
        :action="refreshPage"
        :loading="loading || refreshing"
        :disabled="loading || refreshing"
      />
    </header>

    <div v-if="loading" class="state-message">{{ t('加载中') }}</div>
    <div v-else-if="error" class="state-message error">{{ error }}</div>
    <template v-else>
      <section class="security-section">
        <div class="section-head">
          <h2>{{ t('管理员') }}</h2>
          <div class="section-actions">
            <span>{{ admins.length }}</span>
            <button v-if="canManageAdmins" class="primary-button" type="button" :disabled="operationPending" @click="openAdminCreateDialog">
              {{ t('创建管理员') }}
            </button>
          </div>
        </div>
        <div v-if="!admins.length" class="state-message">{{ t('暂无数据') }}</div>
        <ul v-else class="security-list">
          <li v-for="admin in admins" :key="admin.id" class="security-row">
            <div>
              <strong>{{ admin.display_name || admin.username }}</strong>
              <small>{{ admin.username }} · {{ admin.enabled ? t('已启用') : t('已禁用') }}</small>
              <small>{{ t('角色') }}：{{ admin.role_keys.join(', ') || t('无') }}</small>
              <small>{{ t('权限') }}：{{ admin.permission_keys.join(', ') || t('无') }}</small>
              <small>
                {{ t('服务器授权') }}：
                {{ admin.server_grants.map((grant) => `${grant.server_name || grant.server_id} (${grant.scope})`).join(', ') || t('无') }}
              </small>
            </div>
            <div class="row-actions">
              <div class="row-meta">
                <span>{{ admin.totp_enabled ? t('2FA 已启用') : t('2FA 未启用') }}</span>
                <span>{{ admin.id === currentUserId ? t('当前账号') : '' }}</span>
              </div>
              <template v-if="canManageAdmins">
                <UiSwitch
                  :model-value="admin.enabled"
                  :label="admin.enabled ? t('禁用管理员') : t('启用管理员')"
                  :disabled="operationPending || admin.id === currentUserId"
                  size="compact"
                  @update:model-value="setAdminEnabled(admin, $event)"
                />
                <button class="secondary-button" type="button" :disabled="operationPending" @click="openRoleEditor(admin)">{{ t('编辑角色') }}</button>
                <button class="secondary-button" type="button" :disabled="operationPending" @click="openGrantEditor(admin)">{{ t('编辑服务器授权') }}</button>
                <button class="secondary-button" type="button" :disabled="operationPending" @click="resetAdminPassword(admin)">{{ t('重置密码') }}</button>
                <button class="danger-button" type="button" :disabled="operationPending || admin.id === currentUserId" @click="deleteAdmin(admin)">{{ t('删除') }}</button>
              </template>
            </div>
          </li>
        </ul>
        <details v-if="roles.length" class="roles-details">
          <summary>{{ t('可用角色') }}</summary>
          <ul class="security-list compact-list">
            <li v-for="role in roles" :key="role.id">
              <strong>{{ role.key }}</strong>
              <span>{{ role.permissions.join(', ') || t('无权限') }}</span>
            </li>
          </ul>
        </details>
      </section>

      <section class="security-section">
        <div class="section-head">
          <h2>{{ t('API Token') }}</h2>
          <button v-if="canWriteSecurity" class="primary-button" type="button" :disabled="operationPending" @click="openTokenCreateDialog">
            {{ t('创建 Token') }}
          </button>
        </div>
        <ul v-if="tokens.length" class="security-list">
          <li v-for="token in tokens" :key="token.id" class="security-row">
            <div>
              <strong>{{ token.name }}</strong>
              <small>{{ token.token_prefix }} · {{ token.permissions.join(', ') }}</small>
              <small>{{ t('服务器') }}：{{ token.server_ids?.join(', ') || t('跟随账号授权') }}</small>
              <small>{{ t('最近使用') }}：{{ timestamp(token.last_used_at) }}</small>
            </div>
            <button v-if="canWriteSecurity" class="danger-button" type="button" :disabled="operationPending || Boolean(token.revoked_at)" @click="revokeToken(token.id)">
              {{ token.revoked_at ? t('已撤销') : t('撤销') }}
            </button>
          </li>
        </ul>
        <div v-else class="state-message">{{ t('暂无数据') }}</div>
      </section>

      <section class="security-section">
        <div class="section-head"><h2>{{ t('登录会话') }}</h2><span>{{ sessions.length }}</span></div>
        <ul v-if="sessions.length" class="security-list">
          <li v-for="session in sessions" :key="session.id" class="security-row">
            <div>
              <strong>{{ session.device_label || session.user_agent || t('未知设备') }}</strong>
              <small>{{ session.ip || t('未知地址') }} · {{ session.auth_method }}</small>
              <small>{{ t('最近活动') }}：{{ timestamp(session.last_seen_at) }} · {{ t('到期') }}：{{ timestamp(session.expires_at) }}</small>
            </div>
            <button v-if="canWriteSecurity" class="danger-button" type="button" :disabled="operationPending || Boolean(session.revoked_at)" @click="revokeSession(session.id)">
              {{ session.revoked_at ? t('已撤销') : t('撤销') }}
            </button>
          </li>
        </ul>
        <div v-else class="state-message">{{ t('暂无数据') }}</div>
      </section>

      <section class="security-section two-factor-section">
        <div class="section-head"><h2>{{ t('2FA 设置') }}</h2></div>
        <p v-if="!twoFactor.login_challenge_available" class="state-message warning">
          {{ t('当前登录界面尚未支持 2FA 验证，暂不能启用') }}
        </p>
        <div class="two-factor-actions">
          <UiSwitch
            v-if="canWriteSecurity"
            :model-value="twoFactor.enabled"
            :label="t('2FA 已启用')"
            :disabled="!twoFactor.enabled && (!twoFactor.available || !twoFactor.login_challenge_available) || operationPending"
            @update:model-value="($event) => $event ? startTwoFactor() : disableTwoFactor()"
          />
          <button v-if="canWriteSecurity" class="primary-button" type="button" :disabled="operationPending || twoFactor.enabled || !twoFactor.available || !twoFactor.login_challenge_available" @click="startTwoFactor">{{ t('启用 2FA') }}</button>
          <button v-if="canWriteSecurity" class="secondary-button" type="button" :disabled="operationPending || !twoFactor.enabled" @click="disableTwoFactor">{{ t('禁用 2FA') }}</button>
          <button v-if="canWriteSecurity" class="secondary-button" type="button" :disabled="operationPending || !twoFactor.enabled" @click="regenerateRecoveryCodes">{{ t('重新生成恢复码') }}</button>
          <button v-if="canWriteSecurity" class="secondary-button" type="button" :disabled="operationPending || !twoFactor.pending" @click="rollbackTwoFactor">{{ t('回滚') }}</button>
        </div>
        <small>{{ t('剩余恢复码') }}：{{ twoFactor.recovery_codes_remaining }}</small>
      </section>
    </template>

    <SettingsDialogShell
      :open="canManageAdmins && showAdminCreateDialog"
      :title="t('创建管理员')"
      :description="t('账号、角色与服务器授权将作为一个事务创建')"
      :close-label="t('关闭')"
      :show-description="true"
      @update:open="showAdminCreateDialog = $event"
    >
      <div class="dialog-form">
        <label>{{ t('用户名') }}<input v-model="adminCreate.username" type="text" autocomplete="off" /></label>
        <label>{{ t('显示名称') }}<input v-model="adminCreate.displayName" type="text" autocomplete="off" /></label>
        <label>{{ t('初始密码') }}<input v-model="adminCreate.password" type="password" autocomplete="new-password" /></label>
        <fieldset class="choice-group">
          <legend>{{ t('角色') }}</legend>
          <CheckboxField
            v-for="role in roles"
            :key="role.id"
            :model-value="adminCreate.roleKeys.includes(role.key)"
            :label="`${role.name} (${role.key})`"
            @update:model-value="adminCreate.roleKeys = updateSelection(adminCreate.roleKeys, role.key, $event)"
          />
        </fieldset>
        <fieldset class="choice-group">
          <legend>{{ t('服务器授权') }}</legend>
          <div v-if="!serverOptions.length" class="state-message">{{ t('暂无服务器') }}</div>
          <CheckboxField
            v-for="server in serverOptions"
            :key="server.id"
            :model-value="adminCreate.serverIds.includes(server.id)"
            :label="`${server.name} (${server.id})`"
            @update:model-value="adminCreate.serverIds = updateSelection(adminCreate.serverIds, server.id, $event)"
          />
        </fieldset>
      </div>
      <template #footer>
        <button class="secondary-button" type="button" @click="showAdminCreateDialog = false">{{ t('取消') }}</button>
        <button class="primary-button" type="button" :disabled="operationPending || !adminCreate.username.trim() || adminCreate.password.length < 4 || !adminCreate.roleKeys.length" @click="createAdmin">{{ t('创建') }}</button>
      </template>
    </SettingsDialogShell>

    <SettingsDialogShell
      :open="canManageAdmins && showRoleDialog"
      :title="t('编辑角色')"
      :description="selectedAdmin ? `${selectedAdmin.display_name || selectedAdmin.username} · ${t('至少选择一个角色')}` : ''"
      :close-label="t('关闭')"
      :show-description="true"
      @update:open="showRoleDialog = $event"
    >
      <div class="dialog-form choice-list">
        <CheckboxField
          v-for="role in roles"
          :key="role.id"
          :model-value="roleDraft.includes(role.key)"
          :label="`${role.name} (${role.key})`"
          @update:model-value="roleDraft = updateSelection(roleDraft, role.key, $event)"
        />
      </div>
      <template #footer>
        <button class="secondary-button" type="button" @click="showRoleDialog = false">{{ t('取消') }}</button>
        <button class="primary-button" type="button" :disabled="operationPending || !roleDraft.length" @click="saveRoles">{{ t('保存角色') }}</button>
      </template>
    </SettingsDialogShell>

    <SettingsDialogShell
      :open="canManageAdmins && showGrantDialog"
      :title="t('编辑服务器授权')"
      :description="selectedAdmin ? selectedAdmin.display_name || selectedAdmin.username : ''"
      :close-label="t('关闭')"
      :show-description="true"
      @update:open="showGrantDialog = $event"
    >
      <div class="dialog-form choice-list">
        <div v-if="!serverOptions.length" class="state-message">{{ t('暂无服务器') }}</div>
        <CheckboxField
          v-for="server in serverOptions"
          :key="server.id"
          :model-value="serverGrantDraft.includes(server.id)"
          :label="`${server.name} (${server.id})`"
          @update:model-value="serverGrantDraft = updateSelection(serverGrantDraft, server.id, $event)"
        />
      </div>
      <template #footer>
        <button class="secondary-button" type="button" @click="showGrantDialog = false">{{ t('取消') }}</button>
        <button class="primary-button" type="button" :disabled="operationPending" @click="saveServerGrants">{{ t('保存服务器授权') }}</button>
      </template>
    </SettingsDialogShell>

    <SettingsDialogShell
      :open="canWriteSecurity && showTokenCreateDialog"
      :title="t('创建 API Token')"
      :description="t('Token 明文只会显示一次')"
      :close-label="t('关闭')"
      :show-description="true"
      @update:open="showTokenCreateDialog = $event"
    >
      <div class="dialog-form">
        <label>{{ t('名称') }}<input v-model="dto.tokenName" type="text" autocomplete="off" /></label>
        <label>{{ t('权限') }}<input v-model="dto.tokenPermissions" type="text" :placeholder="t('用逗号分隔权限')" /></label>
        <label>{{ t('服务器 ID') }}<input v-model="dto.tokenServerIds" type="text" :placeholder="t('可选，用逗号分隔')" /></label>
        <label>{{ t('过期时间') }}<input v-model="dto.tokenExpiresAt" type="datetime-local" /></label>
      </div>
      <template #footer>
        <button class="secondary-button" type="button" @click="showTokenCreateDialog = false">{{ t('取消') }}</button>
        <button class="primary-button" type="button" :disabled="operationPending || !dto.tokenName.trim()" @click="createToken">{{ t('创建') }}</button>
      </template>
    </SettingsDialogShell>

    <SettingsDialogShell
      :open="showTokenResultDialog"
      :title="t('Token 创建成功')"
      :description="t('请立即复制并安全保存，关闭后不会再次显示')"
      :close-label="t('关闭')"
      :show-description="true"
      @update:open="closeTokenResult"
    >
      <div v-if="createdToken" class="secret-result">
        <code>{{ createdToken.token }}</code>
        <small>{{ createdToken.token_prefix }}</small>
      </div>
    </SettingsDialogShell>

    <SettingsDialogShell
      :open="canWriteSecurity && showTwoFactorDialog"
      :title="t('2FA 设置')"
      :description="t('将密钥加入验证器后输入验证码确认')"
      :close-label="t('关闭')"
      :show-description="true"
      @update:open="showTwoFactorDialog = $event"
    >
      <div v-if="pendingTwoFactor" class="secret-result">
        <label>{{ t('密钥') }}<code>{{ pendingTwoFactor.secret }}</code></label>
        <label>{{ t('配置 URI') }}<code>{{ pendingTwoFactor.otpauth_uri }}</code></label>
      </div>
      <template #footer>
        <button class="secondary-button" type="button" @click="showTwoFactorDialog = false">{{ t('取消') }}</button>
        <button class="primary-button" type="button" :disabled="operationPending" @click="confirmTwoFactor">{{ t('确认启用') }}</button>
      </template>
    </SettingsDialogShell>

    <SettingsDialogShell
      :open="showRecoveryDialog"
      :title="t('恢复码')"
      :description="t('恢复码只显示一次，请离线保存')"
      :close-label="t('关闭')"
      :show-description="true"
      @update:open="closeRecoveryDialog"
    >
      <div class="recovery-codes">
        <code v-for="code in recoveryCodes" :key="code">{{ code }}</code>
      </div>
    </SettingsDialogShell>
  </div>
</template>

<style scoped>
.admin-security-page { display: grid; gap: 1rem; }
.admin-security-page, .dialog-form, .secret-result, .recovery-codes { font-size: var(--font-size-body); line-height: 1.6; }
.page-head, .section-head, .security-row, .two-factor-actions, .section-actions { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: .75rem; }
.page-head h1 { margin: 0; font-size: var(--font-size-page); font-weight: 700; line-height: 1.3; }
.section-head h2 { font-size: var(--font-size-section); font-weight: 700; line-height: 1.4; }
.page-head p { margin: .25rem 0 0; }
.page-head p, .security-row small, .state-message, .roles-details, .two-factor-section > small, .secret-result small { color: var(--muted); }
.page-head p, .security-row small, .state-message, .section-head > span, .section-actions > span, .compact-list span, .two-factor-section > small, .secret-result small { font-size: 12px; line-height: 1.6; }
.security-row strong, .compact-list strong { font-size: 13px; font-weight: 600; }
.security-section { display: grid; gap: .75rem; }
.security-list { display: grid; gap: .5rem; list-style: none; padding: 0; margin: 0; }
.security-row { align-items: flex-start; border: 1px solid var(--border); border-radius: .5rem; padding: .75rem; }
.security-row > div:first-child { display: grid; flex: 1 1 16rem; gap: .25rem; min-width: 0; overflow-wrap: anywhere; }
.row-actions { display: flex; align-items: center; justify-content: flex-end; flex-wrap: wrap; gap: .5rem; min-width: 0; max-width: 100%; }
.row-meta { display: grid; gap: .25rem; min-width: 0; text-align: right; color: var(--muted); font-size: var(--font-size-meta); line-height: 1.6; overflow-wrap: anywhere; }
.compact-list { margin-top: .75rem; }
.compact-list li { display: flex; justify-content: space-between; flex-wrap: wrap; gap: 1rem; }
.compact-list li > * { min-width: 0; overflow-wrap: anywhere; }
.state-message.error { color: var(--danger, #b42318); }
.state-message.warning { color: var(--warning, #a15c00); }
.dialog-form { display: grid; gap: .75rem; padding: 1rem; }
.dialog-form > label:not(.check), .secret-result label { display: grid; gap: .35rem; min-width: 0; color: var(--muted-strong); font-size: var(--font-size-label); font-weight: 700; }
.dialog-form input { min-width: 0; font-weight: 400; }
.choice-group { display: grid; gap: .5rem; min-width: 0; margin: 0; padding: .75rem; border: 1px solid var(--border); border-radius: .5rem; }
.choice-group legend { color: var(--muted-strong); font-size: 11px; font-weight: 700; }
.dialog-form :deep(.check) { min-width: 0; }
.dialog-form :deep(.check input) { flex-shrink: 0; }
.dialog-form :deep(.check span) { min-width: 0; overflow-wrap: anywhere; }
.choice-list { align-content: start; }
.secret-result, .recovery-codes { display: grid; gap: .75rem; padding: 1rem; }
.secret-result code, .recovery-codes code { min-width: 0; color: var(--text); font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; font-weight: 400; overflow-wrap: anywhere; user-select: all; white-space: pre-wrap; }
.primary-button, .secondary-button, .danger-button { border: 1px solid var(--border); border-radius: .4rem; padding: .45rem .7rem; font-size: 12px; font-weight: 600; line-height: 1.4; cursor: pointer; }
.primary-button { background: var(--primary, #2563eb); color: white; }
.secondary-button { background: var(--button-bg); color: var(--button-text); }
.danger-button { color: var(--danger, #b42318); }
button:disabled:not(.refresh-button) { cursor: not-allowed; opacity: .55; }
@media (max-width: 700px) {
  .page-head, .section-head { align-items: flex-start; }
  .security-row > div:first-child { flex-basis: 100%; }
  .row-actions { width: 100%; justify-content: flex-start; }
  .row-meta { text-align: left; }
  .compact-list li { gap: .35rem; }
  .compact-list li > * { flex-basis: 100%; }
}
</style>
