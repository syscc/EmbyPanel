<script setup lang="ts">
import {
  Activity,
  Check,
  Copy,
  KeyRound,
  Plus,
  RefreshCw,
  Search,
  Settings2,
  ShieldCheck,
  Trash2,
  Users,
  X,
} from '@lucide/vue'
import { computed, onMounted, reactive, ref, watch } from 'vue'
import { usePanelContext } from '@/composables/panel-context'
import { useUsersController } from '@/composables/useUsersController'
import UserPolicyFields from '@/components/UserPolicyFields.vue'
import SettingsDialogShell from '@/components/ui/SettingsDialogShell.vue'
import UiSwitch from '@/components/ui/UiSwitch.vue'
import {
  applyTemplateTo,
  copyPolicy,
  defaultPolicyDraft,
  policyFromUser,
  policyPayload,
} from '@/lib/user-policy'
import type { UserPolicyDraft, UserSummary, UserTemplate } from '@/types/panel'

const panel = usePanelContext()
const { api, encryptPayload, t, showNotice, clearNotice, confirmAction, formatTimestamp } = panel
const controller = useUsersController({ api, encryptPayload })
const {
  users,
  visibleUsers,
  templates,
  servers,
  serverErrors,
  loading,
  templateLoading,
  saving,
  error,
  search,
  serverFilter,
  statusFilter,
  refresh,
  updatePolicy,
  resetPassword,
  createUser,
  deleteUser,
  saveTemplate,
  deleteTemplate,
  clearFilters,
} = controller

const selectedUser = ref<UserSummary | null>(null)
const dialogOpen = ref(false)
const createDialogOpen = ref(false)
const templateDialogOpen = ref(false)
const createName = ref('')
const createPassword = ref('')
const createServerId = ref('')
const createTemplateId = ref('')
const templateName = ref('')
const templateServerId = ref('')
const editingTemplateId = ref('')
const selectedTemplateId = ref('')
const draft = reactive(defaultPolicyDraft())
const createDraft = reactive(defaultPolicyDraft())
const templateDraft = reactive(defaultPolicyDraft())
const newPassword = ref('')

const availableTemplates = computed(() =>
  templates.value.filter((template) => template.server_id === (selectedUser.value?.server_id || templateServerId.value)),
)
const createTemplates = computed(() => templates.value.filter((template) => template.server_id === createServerId.value))
const templateFolders = computed(() => collectAccessOptions(templateServerId.value, 'available_folders'))
const templateDevices = computed(() => collectAccessOptions(templateServerId.value, 'available_devices'))
const createFolders = computed(() => collectAccessOptions(createServerId.value, 'available_folders'))
const createDevices = computed(() => collectAccessOptions(createServerId.value, 'available_devices'))

function serverName(serverId: string) {
  return servers.value.find((server) => server.id === serverId)?.name || serverId
}

function openEditor(user: UserSummary) {
  error.value = ''
  clearNotice()
  selectedUser.value = user
  copyPolicy(draft, policyFromUser(user))
  selectedTemplateId.value = ''
  newPassword.value = ''
  dialogOpen.value = true
}

async function savePolicy() {
  if (!selectedUser.value) return
  clearNotice()
  try {
    const updated = await updatePolicy(selectedUser.value, policyPayload(draft))
    selectedUser.value = updated
    showNotice(t('用户策略已保存'))
  } catch {
    // The shared controller keeps the actionable error message.
  }
}

function openCreate() {
  error.value = ''
  clearNotice()
  createName.value = ''
  createPassword.value = ''
  createServerId.value = servers.value[0]?.id || ''
  createTemplateId.value = ''
  copyPolicy(createDraft, defaultPolicyDraft())
  createDialogOpen.value = true
}

function applyCreateTemplate() {
  const template = templates.value.find(
    (item) => item.server_id === createServerId.value && item.id === createTemplateId.value,
  )
  copyPolicy(createDraft, defaultPolicyDraft())
  applyTemplateTo(createDraft, template || null)
}

function applySelectedTemplate() {
  applyTemplateTo(
    draft,
    availableTemplates.value.find((template) => template.id === selectedTemplateId.value) || null,
  )
}

async function submitCreate() {
  if (!createServerId.value || createName.value.trim().length < 1 || createPassword.value.length < 4) return
  clearNotice()
  try {
    await createUser(createServerId.value, {
      name: createName.value.trim(),
      new_password: createPassword.value,
      template_id: createTemplateId.value || undefined,
      policy: policyPayload(createDraft),
    })
    createPassword.value = ''
    createDialogOpen.value = false
    showNotice(t('用户已创建'))
  } catch {
    // The shared controller keeps the actionable error message.
  }
}

function openTemplateEditor(serverId: string, source?: UserPolicyDraft, template?: UserTemplate) {
  error.value = ''
  clearNotice()
  templateServerId.value = serverId
  editingTemplateId.value = template?.id || ''
  templateName.value = template?.name || ''
  if (template) copyPolicy(templateDraft, template.policy)
  else if (source) copyPolicy(templateDraft, source)
  else copyPolicy(templateDraft, defaultPolicyDraft())
  templateDialogOpen.value = true
}

async function submitTemplate() {
  if (!templateServerId.value || !templateName.value.trim()) return
  clearNotice()
  try {
    await saveTemplate(templateServerId.value, {
      id: editingTemplateId.value || undefined,
      name: templateName.value.trim(),
      policy: policyPayload(templateDraft),
    })
    templateDialogOpen.value = false
    showNotice(t('模板已保存'))
  } catch {
    // The shared controller keeps the actionable error message.
  }
}

async function askDelete(user: UserSummary) {
  if (saving.value) return
  error.value = ''
  clearNotice()
  const confirmed = await confirmAction({
    title: t('删除用户'),
    description: `${user.name}：${t('删除操作不可撤销')}`,
    confirmText: t('确认删除'),
    cancelText: t('取消'),
    tone: 'danger',
  })
  if (!confirmed || saving.value) return
  try {
    await deleteUser(user)
    dialogOpen.value = false
    selectedUser.value = null
    showNotice(t('用户已删除'))
  } catch {
    // The shared controller keeps the actionable error message.
  }
}

async function askDeleteTemplate(template: UserTemplate) {
  if (templateLoading.value) return
  error.value = ''
  clearNotice()
  const confirmed = await confirmAction({
    title: t('删除模板'),
    description: `${template.name}：${t('确认删除模板')}`,
    confirmText: t('确认删除'),
    cancelText: t('取消'),
    tone: 'danger',
  })
  if (!confirmed || templateLoading.value) return
  try {
    await deleteTemplate(template)
    showNotice(t('模板已删除'))
  } catch {
    // The shared controller keeps the actionable error message.
  }
}

function collectAccessOptions(serverId: string, key: 'available_folders' | 'available_devices') {
  const options = new Map<string, string>()
  for (const user of users.value) {
    if (user.server_id !== serverId) continue
    for (const item of user[key]) options.set(item.id, item.name)
  }
  return Array.from(options, ([id, name]) => ({ id, name }))
}

async function savePassword() {
  if (!selectedUser.value || newPassword.value.trim().length < 4) return
  clearNotice()
  try {
    await resetPassword(selectedUser.value, newPassword.value)
    newPassword.value = ''
    showNotice(t('用户密码已重置'))
  } catch {
    // The shared controller keeps the actionable error message.
  }
}

async function toggleUserEnabled(user: UserSummary) {
  if (saving.value) return
  clearNotice()
  try {
    await updatePolicy(user, {
      ...policyFromUser(user),
      is_disabled: !user.is_disabled,
    })
    showNotice(t(user.is_disabled ? '用户已启用' : '用户已禁用'))
  } catch {
    // The shared controller keeps the actionable error message.
  }
}

watch(createDialogOpen, (open) => {
  if (!open) createPassword.value = ''
})

watch(dialogOpen, (open) => {
  if (!open) newPassword.value = ''
})

onMounted(() => void refresh())
</script>

<template>
  <section class="users-page">
    <section class="panel users-panel">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <Users :size="18" />
            <h2>{{ t('用户中心') }}</h2>
          </div>
          <p class="muted">{{ t('跨服务器用户与访问策略') }}</p>
        </div>
        <div class="panel-actions">
          <button class="secondary" type="button" :disabled="loading" @click="refresh">
            <RefreshCw :size="15" />{{ loading ? t('加载中') : t('刷新') }}
          </button>
          <button class="secondary" type="button" :disabled="!servers.length || templateLoading" @click="openTemplateEditor(servers[0]?.id || '')">
            <Copy :size="15" />{{ t('新建模板') }}
          </button>
          <button class="primary" type="button" :disabled="!servers.length || saving" @click="openCreate">
            <Plus :size="15" />{{ t('新增用户') }}
          </button>
        </div>
      </div>

      <div class="users-toolbar">
        <label class="users-search">
          <span class="sr-only">{{ t('搜索用户') }}</span>
          <Search :size="16" />
          <input v-model="search" :placeholder="t('搜索用户或服务器')" @keyup.enter="refresh" />
        </label>
        <label>
          <span>{{ t('服务器') }}</span>
          <select v-model="serverFilter">
            <option value="all">{{ t('全部服务器') }}</option>
            <option v-for="server in servers" :key="server.id" :value="server.id">{{ server.name }}</option>
          </select>
        </label>
        <label>
          <span>{{ t('状态') }}</span>
          <select v-model="statusFilter">
            <option value="all">{{ t('全部') }}</option>
            <option value="enabled">{{ t('已启用') }}</option>
            <option value="disabled">{{ t('已禁用') }}</option>
          </select>
        </label>
        <button class="secondary" type="button" @click="clearFilters">
          <X :size="15" />{{ t('清空筛选') }}
        </button>
      </div>

      <div v-if="templates.length" class="users-templates">
        <div class="users-section-title"><Copy :size="16" />{{ t('权限模板') }}</div>
        <div class="users-template-list">
          <div v-for="template in templates" :key="`${template.server_id}:${template.id}`" class="users-template-row">
            <div>
              <strong>{{ template.name }}</strong>
              <small>{{ serverName(template.server_id) }}</small>
            </div>
            <div class="users-template-actions">
              <button class="secondary icon-button" type="button" :aria-label="t('编辑模板')" @click="openTemplateEditor(template.server_id, undefined, template)">
                <Settings2 :size="15" />
              </button>
              <button class="danger-button icon-button" type="button" :aria-label="t('删除模板')" @click="askDeleteTemplate(template)">
                <Trash2 :size="15" />
              </button>
            </div>
          </div>
        </div>
      </div>

      <div v-if="error" class="notice error" role="alert">{{ error }}</div>
      <div v-for="item in serverErrors" :key="item.server_id" class="notice warning" role="status">
        {{ item.server_name }}: {{ item.error }}
      </div>
      <div v-if="loading && !visibleUsers.length" class="empty-state compact">
        <Activity :size="18" />{{ t('加载中') }}
      </div>
      <div v-else-if="!visibleUsers.length" class="empty-state compact">{{ t('暂无用户') }}</div>
      <div v-else class="users-table-wrap">
        <table class="users-table">
          <colgroup>
            <col class="users-col-user" />
            <col class="users-col-server" />
            <col class="users-col-status" />
            <col class="users-col-activity" />
            <col class="users-col-devices" />
            <col class="users-col-streams" />
            <col class="users-col-actions" />
          </colgroup>
          <thead>
            <tr>
              <th>{{ t('用户') }}</th>
              <th>{{ t('服务器') }}</th>
              <th>{{ t('状态') }}</th>
              <th>{{ t('最近活动') }}</th>
              <th>{{ t('设备') }}</th>
              <th>{{ t('同时播放') }}</th>
              <th>{{ t('操作') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="user in visibleUsers" :key="`${user.server_id}:${user.user_id}`">
              <td class="users-cell-primary" :data-label="t('用户')">
                <div class="users-user-name-line">
                  <strong>{{ user.name }}</strong>
                  <span v-if="user.is_administrator" class="user-admin-badge" :title="t('管理员')">
                    <ShieldCheck :size="12" />{{ t('管理员') }}
                  </span>
                </div>
                <small class="users-user-id" :title="user.user_id">{{ user.user_id }}</small>
              </td>
              <td :data-label="t('服务器')"><span class="users-cell-ellipsis" :title="user.server_name">{{ user.server_name }}</span></td>
              <td :data-label="t('状态')">
                <span class="user-status" :class="{ disabled: user.is_disabled }">
                  {{ user.is_disabled ? t('已禁用') : t('已启用') }}
                </span>
              </td>
              <td :data-label="t('最近活动')">
              <span>{{ user.last_activity ? formatTimestamp(user.last_activity) : '--' }}</span>
                <small>{{ user.active_sessions }} {{ t('个活动会话') }}</small>
              </td>
              <td :data-label="t('设备')">
                <span class="users-cell-ellipsis" :title="user.devices.join('、')">{{ user.devices.length ? user.devices.join('、') : '--' }}</span>
              </td>
              <td :data-label="t('同时播放')">
                {{ user.user_policy.concurrent_playback_limit_enabled
                  ? user.user_policy.concurrent_playback_limit_max
                  : (user.simultaneous_stream_limit ?? '∞') }}
              </td>
              <td class="users-cell-actions" :data-label="t('操作')">
                <div class="users-row-actions">
                  <UiSwitch
                    class="user-enabled-switch"
                    :model-value="!user.is_disabled"
                    :label="t(user.is_disabled ? '启用用户' : '禁用用户')"
                    :disabled="saving"
                    @update:model-value="toggleUserEnabled(user)"
                  />
                  <button class="secondary icon-text" type="button" :disabled="saving" @click="openEditor(user)">
                    <Settings2 :size="15" />{{ t('策略') }}
                  </button>
                  <button class="danger-button icon-button" type="button" :aria-label="t('删除用户')" :title="t('删除用户')" :disabled="saving" @click="askDelete(user)">
                    <Trash2 :size="15" />
                  </button>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>

    <SettingsDialogShell
      v-if="selectedUser"
      v-model:open="dialogOpen"
      :title="selectedUser.name"
      :description="`${selectedUser.server_name} · ${selectedUser.user_id}`"
      :close-label="t('关闭')"
      content-class="users-dialog-content"
      show-description
    >
      <template #icon>
        <ShieldCheck :size="19" />
      </template>

          <form id="user-policy-form" class="users-policy-form" @submit.prevent="savePolicy">
            <div v-if="error" class="notice error" role="alert">{{ error }}</div>
            <div class="users-template-apply">
              <label>
                <span>{{ t('套用权限模板') }}</span>
                <select v-model="selectedTemplateId" @change="applySelectedTemplate">
                  <option value="">{{ t('不套用模板') }}</option>
                  <option v-for="template in availableTemplates" :key="template.id" :value="template.id">{{ template.name }}</option>
                </select>
              </label>
              <button class="secondary" type="button" :disabled="templateLoading" @click="openTemplateEditor(selectedUser.server_id, draft)">
                <Copy :size="15" />{{ t('从当前策略新建模板') }}
              </button>
            </div>
            <UserPolicyFields :draft="draft" :folders="selectedUser.available_folders" :devices="selectedUser.available_devices" />
          </form>

          <form class="users-password-form" @submit.prevent="savePassword">
            <div class="users-section-title"><KeyRound :size="16" />{{ t('重置用户密码') }}</div>
            <div class="users-password-row">
              <input v-model="newPassword" type="password" minlength="4" autocomplete="new-password" :placeholder="t('输入临时密码（至少 4 位）')" />
              <button class="secondary" type="submit" :disabled="saving || newPassword.trim().length < 4"><KeyRound :size="15" />{{ t('重置密码') }}</button>
            </div>
          </form>
      <template #footer>
        <button class="secondary" type="button" @click="dialogOpen = false">{{ t('取消') }}</button>
        <button class="danger-button" type="button" :disabled="saving" @click="askDelete(selectedUser)"><Trash2 :size="15" />{{ t('删除用户') }}</button>
        <button class="primary" type="submit" form="user-policy-form" :disabled="saving"><Check :size="15" />{{ saving ? t('保存中') : t('保存策略') }}</button>
      </template>
    </SettingsDialogShell>

    <SettingsDialogShell
      v-model:open="createDialogOpen"
      :title="t('新增用户')"
      :description="t('创建 Emby 用户并套用权限模板')"
      :close-label="t('关闭')"
      content-class="users-dialog-content users-create-dialog"
      show-description
    >
      <template #icon>
        <Plus :size="19" />
      </template>
          <form id="user-create-form" class="users-policy-form" @submit.prevent="submitCreate">
            <div v-if="error" class="notice error" role="alert">{{ error }}</div>
            <div class="users-dialog-meta-grid">
              <label><span>{{ t('服务器') }}</span><select v-model="createServerId" @change="createTemplateId = ''; copyPolicy(createDraft, defaultPolicyDraft())"><option v-for="server in servers" :key="server.id" :value="server.id">{{ server.name }}</option></select></label>
              <label><span>{{ t('用户名') }}</span><input v-model="createName" maxlength="128" autocomplete="off" required /></label>
              <label><span>{{ t('初始密码') }}</span><input v-model="createPassword" type="password" minlength="4" maxlength="256" autocomplete="new-password" required /></label>
              <label><span>{{ t('权限模板') }}</span><select v-model="createTemplateId" @change="applyCreateTemplate"><option value="">{{ t('不套用模板') }}</option><option v-for="template in createTemplates" :key="template.id" :value="template.id">{{ template.name }}</option></select></label>
            </div>
            <UserPolicyFields :draft="createDraft" :folders="createFolders" :devices="createDevices" />
          </form>
      <template #footer>
        <button class="secondary" type="button" @click="createDialogOpen = false">{{ t('取消') }}</button>
        <button class="primary" type="submit" form="user-create-form" :disabled="saving || !createServerId || createName.trim().length < 1 || createPassword.length < 4"><Plus :size="15" />{{ saving ? t('创建中') : t('创建用户') }}</button>
      </template>
    </SettingsDialogShell>

    <SettingsDialogShell
      v-model:open="templateDialogOpen"
      :title="editingTemplateId ? t('编辑模板') : t('新建模板')"
      :description="t('模板不包含用户密码')"
      :close-label="t('关闭')"
      content-class="users-dialog-content users-template-dialog"
      show-description
    >
      <template #icon>
        <Copy :size="19" />
      </template>
          <form id="user-template-form" class="users-policy-form" @submit.prevent="submitTemplate">
            <div v-if="error" class="notice error" role="alert">{{ error }}</div>
            <div class="users-dialog-meta-grid">
              <label><span>{{ t('服务器') }}</span><select v-model="templateServerId" :disabled="Boolean(editingTemplateId)"><option v-for="server in servers" :key="server.id" :value="server.id">{{ server.name }}</option></select></label>
              <label><span>{{ t('模板名称') }}</span><input v-model="templateName" maxlength="128" required /></label>
            </div>
            <UserPolicyFields :draft="templateDraft" :folders="templateFolders" :devices="templateDevices" />
          </form>
      <template #footer>
        <button class="secondary" type="button" @click="templateDialogOpen = false">{{ t('取消') }}</button>
        <button class="primary" type="submit" form="user-template-form" :disabled="templateLoading || !templateServerId || !templateName.trim()"><Check :size="15" />{{ templateLoading ? t('保存中') : t('保存模板') }}</button>
      </template>
    </SettingsDialogShell>

  </section>
</template>

<style scoped>
.users-page { display: grid; gap: 16px; }
.users-panel { min-width: 0; }
.users-panel > .panel-head .panel-actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 8px; }
.users-toolbar { display: grid; grid-template-columns: minmax(220px, 1fr) minmax(150px, 220px) minmax(130px, 180px) auto; align-items: end; gap: 12px; margin-bottom: 16px; }
.users-toolbar label, .users-field-grid label { display: grid; gap: 6px; }
.users-toolbar label > span, .users-field-grid label > span { color: var(--muted); font-size: 12px; }
.users-search { display: flex !important; align-items: center; gap: 8px; border: 1px solid var(--border); border-radius: 6px; padding: 0 10px; min-height: 38px; }
.users-search input { border: 0; outline: 0; background: transparent; width: 100%; min-width: 0; }
.users-table-wrap { overflow-x: auto; border: 1px solid var(--border); border-radius: 6px; }
.users-table { width: 100%; min-width: 1040px; border-collapse: collapse; table-layout: fixed; }
.users-col-user { width: 22%; }
.users-col-server { width: 11%; }
.users-col-status { width: 10%; }
.users-col-activity { width: 19%; }
.users-col-devices { width: 13%; }
.users-col-streams { width: 8%; }
.users-col-actions { width: 216px; }
.users-table th, .users-table td { padding: 11px 12px; border-bottom: 1px solid var(--border); text-align: left; vertical-align: middle; }
.users-table th { color: var(--muted); font-size: 12px; font-weight: 600; white-space: nowrap; }
.users-table td { font-size: 13px; }
.users-table tbody tr:last-child td { border-bottom: 0; }
.users-table td small, .users-table td > span + small { display: block; color: var(--muted); margin-top: 3px; font-size: 11px; }
.users-user-name-line { display: flex; align-items: center; gap: 6px; min-width: 0; }
.users-user-name-line strong { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.users-user-id, .users-cell-ellipsis { display: block; max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.users-user-id { font-variant-numeric: tabular-nums; }
.user-status { color: var(--success-text); }
.user-status.disabled { color: var(--danger-text); }
.user-admin-badge { display: inline-flex; flex: 0 0 auto; align-items: center; gap: 3px; padding: 2px 6px; border: 1px solid color-mix(in srgb, var(--accent) 35%, var(--border)); border-radius: 999px; color: var(--accent); font-size: 11px; white-space: nowrap; }
.icon-text { white-space: nowrap; }
.users-row-actions { display: flex; align-items: center; gap: 8px; white-space: nowrap; }
.users-table td:last-child { padding-right: 16px; }
.users-templates { display: grid; gap: 8px; margin-bottom: 16px; padding: 12px; border: 1px solid var(--border); border-radius: 6px; background: var(--subtle-bg); }
.users-template-list { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 8px; }
.users-template-row { display: flex; align-items: center; justify-content: space-between; gap: 8px; min-width: 0; padding: 9px 10px; border: 1px solid var(--border); border-radius: 6px; background: var(--panel-bg); }
.users-template-row strong, .users-template-row small { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.users-template-row small { margin-top: 3px; color: var(--muted); font-size: 11px; }
.users-template-actions { display: flex; flex: 0 0 auto; gap: 5px; }
.users-template-apply { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: end; gap: 10px; }
.users-template-apply label { display: grid; gap: 6px; }
.users-template-apply label > span { color: var(--muted); font-size: 12px; }
.users-policy-form, .users-password-form { display: grid; gap: 14px; padding: 18px; }
.users-dialog-meta-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
.users-dialog-meta-grid label { display: grid; gap: 6px; }
.users-dialog-meta-grid label > span { color: var(--muted); font-size: 12px; }
.users-toggle-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px 16px; }
.users-field-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
.users-field-grid label:last-child:nth-child(odd) { grid-column: 1 / -1; }
.users-access-list { display: grid; align-content: start; gap: 7px; min-height: 92px; max-height: 150px; overflow: auto; padding: 10px; border: 1px solid var(--border); border-radius: 6px; background: var(--subtle-bg); }
.users-access-list.disabled { opacity: .55; }
.users-limit-section, .users-password-form { border-top: 1px solid var(--border); }
.users-section-title { display: flex; align-items: center; gap: 7px; font-weight: 600; font-size: 13px; margin-bottom: 10px; }
.users-password-row { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 10px; }
.notice.warning { color: var(--warning, #a16207); }
@media (max-width: 760px) {
  .users-toolbar { grid-template-columns: 1fr; align-items: stretch; }
  .users-toolbar > * { width: 100%; min-width: 0; }
  .users-search, .users-toolbar > button { grid-column: auto; }
  .users-toolbar > button { justify-self: stretch; min-height: 40px; }
  .users-toggle-grid, .users-field-grid, .users-password-row, .users-dialog-meta-grid { grid-template-columns: 1fr; }
  .users-panel > .panel-head .panel-actions { justify-content: flex-start; width: 100%; }
  .users-panel > .panel-head .panel-actions button { flex: 1 1 120px; min-height: 40px; }
  .users-template-apply { grid-template-columns: 1fr; }
  .users-template-row { align-items: stretch; flex-direction: column; }
  .users-template-actions { width: 100%; }
  .users-template-actions button { flex: 1 1 auto; min-height: 40px; }
  .users-table-wrap { overflow: visible; border: 0; border-radius: 0; background: transparent; }
  .users-table {
    min-width: 0;
    table-layout: auto;
    border-collapse: separate;
    border-spacing: 0 10px;
  }
  .users-table colgroup,
  .users-table thead { display: none; }
  .users-table tbody { display: grid; gap: 0; }
  .users-table tr {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px 12px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel-bg);
  }
  .users-table th,
  .users-table td {
    display: grid;
    gap: 4px;
    width: auto;
    min-width: 0;
    padding: 0;
    border: 0;
    white-space: normal;
    overflow: visible;
    text-overflow: unset;
  }
  .users-table td::before {
    content: attr(data-label);
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
  }
  .users-table td.users-cell-primary,
  .users-table td.users-cell-actions { grid-column: 1 / -1; }
  .users-table td.users-cell-primary::before { display: none; }
  .users-table td.users-cell-actions::before { margin-bottom: 2px; }
  .users-user-name-line strong,
  .users-cell-ellipsis,
  .users-user-id {
    overflow: visible;
    text-overflow: unset;
    white-space: normal;
  }
  .users-row-actions {
    flex-wrap: wrap;
    white-space: normal;
    gap: 8px;
  }
  .users-row-actions .icon-text,
  .users-row-actions .icon-button { min-height: 40px; }
  .users-row-actions .icon-text { flex: 1 1 auto; }
}
@media (max-width: 480px) {
  .users-table tr { grid-template-columns: 1fr; }
}
</style>
