import { computed, onBeforeUnmount, reactive, ref } from 'vue'
import type { Api, EncryptPayload } from '@/composables/useUsersController'

export function useAdminSecurityController(options: { api: Api; encryptPayload: EncryptPayload }) {
  const loading = ref(false)
  const busy = ref('')
  const error = ref('')
  const admins = ref<Admin[]>([])
  const roles = ref<AdminRole[]>([])
  const tokens = ref<AdminToken[]>([])
  const sessions = ref<AdminSession[]>([])
  const currentUserId = ref<number | null>(null)
  const twoFactor = ref<TwoFactorStatus>({
    enabled: false,
    pending: false,
    available: false,
    login_challenge_available: false,
    recovery_codes_remaining: 0
  })
  const createdToken = ref<AdminTokenSecret | null>(null)
  const pendingTwoFactor = ref<TwoFactorSetup | null>(null)
  const recoveryCodes = ref<string[]>([])
  const dto = reactive({
    tokenName: '',
    tokenPermissions: 'admin.security.read',
    tokenServerIds: '',
    tokenExpiresAt: ''
  })

  let loadController: AbortController | null = null
  let operationController: AbortController | null = null
  let loadGeneration = 0
  let disposed = false

  const operationPending = computed(() => busy.value !== '')

  function message(cause: unknown) {
    return cause instanceof Error ? cause.message : String(cause)
  }

  function isAbortError(cause: unknown) {
    return cause instanceof DOMException && cause.name === 'AbortError'
  }

  function ensureActive(signal: AbortSignal) {
    if (disposed || signal.aborted) throw new DOMException('Operation aborted', 'AbortError')
  }

  async function load() {
    if (disposed) return
    const generation = ++loadGeneration
    loadController?.abort()
    const controller = new AbortController()
    loadController = controller
    loading.value = true
    error.value = ''
    try {
      const [adminsResult, tokensResult, sessionsResult, twoFactorResult] = await Promise.allSettled([
        options.api<AdminsResponse>('/api/admins', {
          signal: controller.signal
        }),
        options.api<TokensResponse>('/api/admin/tokens', {
          signal: controller.signal
        }),
        options.api<SessionsResponse>('/api/admin/sessions', {
          signal: controller.signal
        }),
        options.api<TwoFactorStatus>('/api/admin/2fa/status', {
          signal: controller.signal
        })
      ])
      if (disposed || controller.signal.aborted || generation !== loadGeneration) return

      if (adminsResult.status === 'fulfilled') {
        admins.value = adminsResult.value.admins ?? []
        roles.value = adminsResult.value.roles ?? []
        currentUserId.value = adminsResult.value.current_user_id
      }
      if (tokensResult.status === 'fulfilled') tokens.value = tokensResult.value.tokens ?? []
      if (sessionsResult.status === 'fulfilled') sessions.value = sessionsResult.value.sessions ?? []
      if (twoFactorResult.status === 'fulfilled') twoFactor.value = twoFactorResult.value

      const failures = [adminsResult, tokensResult, sessionsResult, twoFactorResult].flatMap((result) =>
        result.status === 'rejected' ? [message(result.reason)] : []
      )
      error.value = [...new Set(failures)].join('; ')
    } catch (cause) {
      if (disposed || controller.signal.aborted || generation !== loadGeneration || isAbortError(cause)) return
      error.value = message(cause)
    } finally {
      if (!disposed && generation === loadGeneration) loading.value = false
      if (loadController === controller) loadController = null
    }
  }

  async function run<T>(name: string, operation: (signal: AbortSignal) => Promise<T>) {
    if (disposed) throw new DOMException('Operation aborted', 'AbortError')
    if (busy.value) throw new Error('operation already in progress')
    const controller = new AbortController()
    operationController = controller
    busy.value = name
    error.value = ''
    try {
      return await operation(controller.signal)
    } catch (cause) {
      if (!disposed && !controller.signal.aborted && !isAbortError(cause)) error.value = message(cause)
      throw cause
    } finally {
      if (operationController === controller) operationController = null
      if (!disposed && busy.value === name) busy.value = ''
    }
  }

  async function createAdmin(input: AdminCreateInput) {
    return run('create-admin', async (signal) => {
      const body = await options.encryptPayload('admin_create', {
        username: input.username.trim(),
        password: input.password,
        display_name: input.displayName.trim() || undefined,
        role_keys: input.roleKeys,
        server_grants: input.serverIds.map((serverId) => ({
          server_id: serverId,
          scope: 'full'
        }))
      })
      ensureActive(signal)
      const response = await options.api<Admin>('/api/admins', {
        method: 'POST',
        body: JSON.stringify(body),
        signal
      })
      ensureActive(signal)
      await load()
      return response
    })
  }

  async function updateAdminRoles(id: number, roleKeys: string[]) {
    return run(`admin-roles-${id}`, async (signal) => {
      const body = await options.encryptPayload('admin_roles', {
        role_keys: roleKeys
      })
      ensureActive(signal)
      await options.api(`/api/admins/${encodeURIComponent(String(id))}/roles`, {
        method: 'PUT',
        body: JSON.stringify(body),
        signal
      })
      ensureActive(signal)
      await load()
    })
  }

  async function updateAdminServerGrants(id: number, serverIds: string[]) {
    return run(`admin-grants-${id}`, async (signal) => {
      const body = await options.encryptPayload('admin_server_grants', {
        server_grants: serverIds.map((serverId) => ({
          server_id: serverId,
          scope: 'full'
        }))
      })
      ensureActive(signal)
      await options.api(`/api/admins/${encodeURIComponent(String(id))}/server-grants`, {
        method: 'PUT',
        body: JSON.stringify(body),
        signal
      })
      ensureActive(signal)
      await load()
    })
  }

  async function setAdminEnabled(id: number, enabled: boolean) {
    return run(`admin-status-${id}`, async (signal) => {
      const body = await options.encryptPayload('admin_status', { enabled })
      ensureActive(signal)
      await options.api(`/api/admins/${encodeURIComponent(String(id))}/status`, {
        method: 'PATCH',
        body: JSON.stringify(body),
        signal
      })
      ensureActive(signal)
      await load()
    })
  }

  async function resetAdminPassword(id: number, newPassword: string) {
    return run(`admin-password-${id}`, async (signal) => {
      const body = await options.encryptPayload('admin_password_reset', {
        new_password: newPassword
      })
      ensureActive(signal)
      await options.api(`/api/admins/${encodeURIComponent(String(id))}/password`, {
        method: 'POST',
        body: JSON.stringify(body),
        signal
      })
      ensureActive(signal)
      await load()
    })
  }

  async function deleteAdmin(id: number) {
    return run(`delete-admin-${id}`, async (signal) => {
      await options.api(`/api/admins/${encodeURIComponent(String(id))}`, {
        method: 'DELETE',
        signal
      })
      ensureActive(signal)
      await load()
    })
  }

  async function createToken() {
    return run('create-token', async (signal) => {
      const permissions = dto.tokenPermissions
        .split(',')
        .map((item) => item.trim())
        .filter(Boolean)
      const serverIds = dto.tokenServerIds
        .split(',')
        .map((item) => item.trim())
        .filter(Boolean)
      const expiresAt = dto.tokenExpiresAt ? Math.floor(new Date(dto.tokenExpiresAt).getTime() / 1000) : undefined
      const body = await options.encryptPayload('admin_token_create', {
        name: dto.tokenName.trim(),
        permissions,
        server_ids: serverIds.length ? serverIds : undefined,
        expires_at: Number.isFinite(expiresAt) ? expiresAt : undefined
      })
      ensureActive(signal)
      const response = await options.api<AdminTokenSecret>('/api/admin/token', {
        method: 'POST',
        body: JSON.stringify(body),
        signal
      })
      ensureActive(signal)
      createdToken.value = response
      dto.tokenName = ''
      dto.tokenServerIds = ''
      dto.tokenExpiresAt = ''
      await load()
      return response
    })
  }

  async function revokeToken(id: number) {
    return run('revoke-token', async (signal) => {
      await options.api(`/api/admin/token/${encodeURIComponent(String(id))}`, {
        method: 'DELETE',
        signal
      })
      ensureActive(signal)
      await load()
    })
  }

  async function revokeSession(id: number) {
    return run('revoke-session', async (signal) => {
      await options.api(`/api/admin/session/${encodeURIComponent(String(id))}`, { method: 'DELETE', signal })
      ensureActive(signal)
      await load()
    })
  }

  async function setup2fa() {
    return run('setup-2fa', async (signal) => {
      const response = await options.api<TwoFactorSetup>('/api/admin/2fa/setup', { method: 'POST', signal })
      ensureActive(signal)
      pendingTwoFactor.value = response
      await load()
      return response
    })
  }

  async function confirm2fa(code: string) {
    return run('confirm-2fa', async (signal) => {
      const body = await options.encryptPayload('admin_2fa_confirm', { code })
      ensureActive(signal)
      const response = await options.api<RecoveryCodesResponse>('/api/admin/2fa/confirm', {
        method: 'POST',
        body: JSON.stringify(body),
        signal
      })
      ensureActive(signal)
      pendingTwoFactor.value = null
      recoveryCodes.value = response.recovery_codes ?? []
      await load()
      return response
    })
  }

  async function disable2fa(password: string, secondFactor: string, recovery?: boolean) {
    return run('disable-2fa', async (signal) => {
      const factor = secondFactor.trim()
      const useRecoveryCode = recovery ?? !/^\d{6}$/.test(factor)
      const body = await options.encryptPayload('admin_2fa_disable', {
        password,
        totp_code: useRecoveryCode ? undefined : factor,
        recovery_code: useRecoveryCode ? factor : undefined
      })
      ensureActive(signal)
      await options.api('/api/admin/2fa/disable', {
        method: 'POST',
        body: JSON.stringify(body),
        signal
      })
      ensureActive(signal)
      clearSensitive()
      await load()
    })
  }

  async function regenerateRecoveryCodes(password: string, secondFactor: string, recovery?: boolean) {
    return run('recovery-codes', async (signal) => {
      const factor = secondFactor.trim()
      const useRecoveryCode = recovery ?? !/^\d{6}$/.test(factor)
      const body = await options.encryptPayload('admin_2fa_recovery', {
        password,
        totp_code: useRecoveryCode ? undefined : factor,
        recovery_code: useRecoveryCode ? factor : undefined
      })
      ensureActive(signal)
      const response = await options.api<RecoveryCodesResponse>('/api/admin/2fa/recovery', {
        method: 'POST',
        body: JSON.stringify(body),
        signal
      })
      ensureActive(signal)
      recoveryCodes.value = response.recovery_codes ?? []
      await load()
      return response
    })
  }

  async function rollback2fa() {
    return run('rollback-2fa', async (signal) => {
      await options.api('/api/admin/2fa/rollback', { method: 'POST', signal })
      ensureActive(signal)
      pendingTwoFactor.value = null
      await load()
    })
  }

  function clearCreatedToken() {
    createdToken.value = null
  }

  function clearRecoveryCodes() {
    recoveryCodes.value = []
  }

  function clearSensitive() {
    createdToken.value = null
    pendingTwoFactor.value = null
    clearRecoveryCodes()
  }

  onBeforeUnmount(() => {
    disposed = true
    loadGeneration += 1
    loadController?.abort()
    loadController = null
    operationController?.abort()
    operationController = null
    clearSensitive()
  })

  return {
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
    busy,
    operationPending,
    error,
    load,
    createAdmin,
    updateAdminRoles,
    updateAdminServerGrants,
    setAdminEnabled,
    resetAdminPassword,
    deleteAdmin,
    createToken,
    revokeToken,
    revokeSession,
    setup2fa,
    confirm2fa,
    disable2fa,
    regenerateRecoveryCodes,
    rollback2fa,
    clearCreatedToken,
    clearRecoveryCodes,
    clearSensitive,
    dto
  }
}

export interface AdminCreateInput {
  username: string
  password: string
  displayName: string
  roleKeys: string[]
  serverIds: string[]
}

interface AdminsResponse {
  current_user_id: number
  admins?: Admin[]
  roles?: AdminRole[]
}

interface TokensResponse {
  tokens?: AdminToken[]
}

interface SessionsResponse {
  sessions?: AdminSession[]
}

interface RecoveryCodesResponse {
  recovery_codes?: string[]
}

export interface AdminServerGrant {
  server_id: string
  server_name?: string | null
  scope: string
  granted_at: number
}

export interface Admin {
  id: number
  username: string
  display_name?: string | null
  enabled: boolean
  disabled_at?: number | null
  totp_enabled: boolean
  totp_pending: boolean
  created_at: string
  updated_at?: number | null
  role_keys: string[]
  permission_keys: string[]
  server_grants: AdminServerGrant[]
}

export interface AdminRole {
  id: number
  key: string
  name: string
  description: string
  is_system: boolean
  permissions: string[]
}

export interface AdminToken {
  id: number
  name: string
  token_prefix: string
  permissions: string[]
  server_ids?: string[] | null
  expires_at?: number | null
  last_used_at?: number | null
  revoked_at?: number | null
  created_at: number
  created_by?: number | null
}

export interface AdminTokenSecret extends AdminToken {
  token: string
}

export interface AdminSession {
  id: number
  ip?: string | null
  user_agent?: string | null
  device_label?: string | null
  last_seen_at: number
  expires_at: number
  created_at: string
  revoked_at?: number | null
  auth_method: string
}

export interface TwoFactorStatus {
  enabled: boolean
  pending: boolean
  available: boolean
  login_challenge_available: boolean
  recovery_codes_remaining: number
}

export interface TwoFactorSetup {
  pending: boolean
  secret: string
  otpauth_uri: string
}

export type AdminSecurityController = ReturnType<typeof useAdminSecurityController>
