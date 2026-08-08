import { computed, ref } from 'vue'
import type {
  UserPolicyInput,
  UserSummary,
  UserTemplate,
  UserTemplatesResponse,
  UsersResponse,
} from '@/types/panel'

type Api = <T>(path: string, init?: RequestInit) => Promise<T>
type EncryptPayload = (name: string, value: unknown) => Promise<unknown>
type UserTemplatePayload = { id?: string; name: string; policy: UserPolicyInput }
type UserCreatePayload = {
  name: string
  new_password: string
  template_id?: string
  policy?: UserPolicyInput
}

export function useUsersController(options: {
  api: Api
  encryptPayload: EncryptPayload
}) {
  const users = ref<UserSummary[]>([])
  const templates = ref<UserTemplate[]>([])
  const serverOptions = ref<Array<{ id: string; name: string }>>([])
  const serverErrors = ref<UsersResponse['server_errors']>([])
  const loading = ref(false)
  const templateLoading = ref(false)
  const saving = ref(false)
  const error = ref('')
  const search = ref('')
  const serverFilter = ref('all')
  const statusFilter = ref<'all' | 'enabled' | 'disabled'>('all')

  const servers = computed(() => {
    return serverOptions.value
  })

  const visibleUsers = computed(() => {
    const keyword = search.value.trim().toLocaleLowerCase()
    return users.value.filter((user) => {
      const matchesServer = serverFilter.value === 'all' || user.server_id === serverFilter.value
      const matchesStatus = statusFilter.value === 'all'
        || (statusFilter.value === 'disabled' ? user.is_disabled : !user.is_disabled)
      const matchesKeyword = !keyword
        || user.name.toLocaleLowerCase().includes(keyword)
        || user.server_name.toLocaleLowerCase().includes(keyword)
      return matchesServer && matchesStatus && matchesKeyword
    })
  })

  async function refresh() {
    loading.value = true
    error.value = ''
    try {
      const [response, templateResponse] = await Promise.all([
        options.api<UsersResponse>('/api/users'),
        options.api<UserTemplatesResponse>('/api/user-templates'),
      ])
      users.value = response.users
      serverOptions.value = response.servers?.length
        ? response.servers
        : Array.from(
            new Map(response.users.map((user) => [user.server_id, user.server_name])),
            ([id, name]) => ({ id, name }),
          )
      serverErrors.value = response.server_errors
      templates.value = templateResponse.templates
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
    } finally {
      loading.value = false
    }
  }

  async function updatePolicy(user: UserSummary, value: Record<string, unknown>) {
    saving.value = true
    error.value = ''
    try {
      const body = await options.encryptPayload('user_policy', value)
      const updated = await options.api<UserSummary>(
        `/api/users/${encodeURIComponent(user.server_id)}/${encodeURIComponent(user.user_id)}/policy`,
        { method: 'PUT', body: JSON.stringify(body) },
      )
      replaceUser(updated)
      error.value = ''
      return updated
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
      throw cause
    } finally {
      saving.value = false
    }
  }

  async function resetPassword(user: UserSummary, newPassword: string) {
    saving.value = true
    error.value = ''
    try {
      const body = await options.encryptPayload('user_password', { new_password: newPassword })
      await options.api<{ success: boolean }>(
        `/api/users/${encodeURIComponent(user.server_id)}/${encodeURIComponent(user.user_id)}/password`,
        { method: 'POST', body: JSON.stringify(body) },
      )
      error.value = ''
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
      throw cause
    } finally {
      saving.value = false
    }
  }

  async function createUser(serverId: string, value: UserCreatePayload) {
    saving.value = true
    error.value = ''
    try {
      const body = await options.encryptPayload('user_create', value)
      const created = await options.api<UserSummary>(
        `/api/users/${encodeURIComponent(serverId)}`,
        { method: 'POST', body: JSON.stringify(body) },
      )
      users.value = [...users.value, created]
      error.value = ''
      return created
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
      throw cause
    } finally {
      saving.value = false
    }
  }

  async function deleteUser(user: UserSummary) {
    saving.value = true
    error.value = ''
    try {
      // Keep the backend confirmation contract without exposing an extra input step in the UI.
      const body = await options.encryptPayload('user_delete', { confirm_name: user.name })
      await options.api<{ success: boolean }>(
        `/api/users/${encodeURIComponent(user.server_id)}/${encodeURIComponent(user.user_id)}`,
        { method: 'DELETE', body: JSON.stringify(body) },
      )
      users.value = users.value.filter((item) => userKey(item) !== userKey(user))
      error.value = ''
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
      throw cause
    } finally {
      saving.value = false
    }
  }

  async function saveTemplate(serverId: string, value: UserTemplatePayload) {
    templateLoading.value = true
    error.value = ''
    try {
      const body = await options.encryptPayload('user_template', value)
      const saved = await options.api<UserTemplate>(
        `/api/user-templates/${encodeURIComponent(serverId)}`,
        { method: 'POST', body: JSON.stringify(body) },
      )
      const index = templates.value.findIndex((item) => item.id === saved.id && item.server_id === saved.server_id)
      if (index >= 0) templates.value.splice(index, 1, saved)
      else templates.value.push(saved)
      error.value = ''
      return saved
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
      throw cause
    } finally {
      templateLoading.value = false
    }
  }

  async function deleteTemplate(template: UserTemplate) {
    templateLoading.value = true
    error.value = ''
    try {
      const body = await options.encryptPayload('user_template_delete', { confirm: true })
      await options.api<{ success: boolean }>(
        `/api/user-templates/${encodeURIComponent(template.server_id)}/${encodeURIComponent(template.id)}`,
        { method: 'DELETE', body: JSON.stringify(body) },
      )
      templates.value = templates.value.filter((item) => item.id !== template.id || item.server_id !== template.server_id)
      error.value = ''
    } catch (cause) {
      error.value = cause instanceof Error ? cause.message : String(cause)
      throw cause
    } finally {
      templateLoading.value = false
    }
  }

  function replaceUser(updated: UserSummary) {
    const index = users.value.findIndex((user) => userKey(user) === userKey(updated))
    if (index >= 0) users.value.splice(index, 1, updated)
  }

  function userKey(user: UserSummary) {
    return `${user.server_id}:${user.user_id}`
  }

  function clearFilters() {
    search.value = ''
    serverFilter.value = 'all'
    statusFilter.value = 'all'
  }

  return {
    users,
    templates,
    visibleUsers,
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
    userKey,
  }
}

export type UsersController = ReturnType<typeof useUsersController>
