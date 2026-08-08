import { ref } from 'vue'
import type { ActionDialogRequest } from '@/composables/useActionDialog'
import type { Settings } from '@/types/panel'

type Api = <T>(path: string, init?: RequestInit) => Promise<T>
type EncryptPayload = (name: string, value: unknown) => Promise<unknown>
type ActionOptions = Omit<ActionDialogRequest, 'kind'>
const MAX_BACKUP_FILE_BYTES = 8 * 1024 * 1024

export function useBackupController(options: {
  api: Api
  encryptPayload: EncryptPayload
  translate: (source: string) => string
  showNotice: (message: string) => void
  clearNotice: () => void
  confirmAction: (options: ActionOptions) => Promise<boolean>
  promptAction: (options: ActionOptions) => Promise<string | null>
  onImported: (settings: Settings) => void
}) {
  const backupError = ref('')
  const backupBusy = ref(false)
  const backupFileInput = ref<HTMLInputElement | null>(null)

  async function exportBackup() {
    if (backupBusy.value) return
    backupBusy.value = true
    backupError.value = ''
    options.clearNotice()
    try {
      const password = await options.promptAction({
        title: options.translate('导出备份'),
        description: options.translate('请输入备份密码（至少 4 位），用于加密配置文件'),
        inputLabel: options.translate('备份密码'),
        inputType: 'password',
        inputAutocomplete: 'new-password',
        confirmText: options.translate('导出备份'),
        cancelText: options.translate('取消'),
        minLength: 4,
      })
      if (password === null) return
      if (password.trim().length < 4) {
        backupError.value = options.translate('备份密码至少需要 4 位')
        return
      }
      const response = await options.api<{ backup: string }>('/api/settings/backup/export', {
        method: 'POST',
        body: JSON.stringify(await options.encryptPayload('backup_export', { password })),
      })
      downloadTextFile(response.backup, backupFileName())
      options.showNotice(options.translate('加密配置备份已生成，请妥善保存备份密码'))
    } catch (error) {
      backupError.value = error instanceof Error ? error.message : String(error)
    } finally {
      backupBusy.value = false
    }
  }

  function importBackup() {
    if (backupBusy.value) return
    backupError.value = ''
    options.clearNotice()
    backupFileInput.value?.click()
  }

  async function handleBackupFileSelected(event: Event) {
    const input = event.target as HTMLInputElement
    const file = input.files?.[0]
    input.value = ''
    if (!file) return
    backupError.value = ''
    options.clearNotice()
    if (file.size > MAX_BACKUP_FILE_BYTES) {
      backupError.value = options.translate('备份文件不能超过 8 MiB')
      return
    }
    backupBusy.value = true
    try {
      await importBackupText(await file.text())
    } catch (error) {
      backupError.value = error instanceof Error ? error.message : String(error)
    } finally {
      backupBusy.value = false
    }
  }

  async function importBackupText(backupText: string) {
    backupError.value = ''
    options.clearNotice()
    const backup = backupText.trim()
    if (!backup) {
      backupError.value = options.translate('配置文件内容为空')
      return
    }
    if (new Blob([backup]).size > MAX_BACKUP_FILE_BYTES) {
      backupError.value = options.translate('备份文件不能超过 8 MiB')
      return
    }
    const confirmed = await options.confirmAction({
      title: options.translate('还原'),
      description: options.translate('还原配置文件会覆盖当前配置，并差量同步受影响的反代监听器，确定继续吗？'),
      confirmText: options.translate('还原'),
      cancelText: options.translate('取消'),
      tone: 'warning',
    })
    if (!confirmed) return
    const encryptedBackup = isEncryptedBackup(backup)
    const password = encryptedBackup
      ? await options.promptAction({
          title: options.translate('还原'),
          description: options.translate('请输入该加密备份的密码'),
          inputLabel: options.translate('备份密码'),
          inputType: 'password',
          inputAutocomplete: 'current-password',
          confirmText: options.translate('继续'),
          cancelText: options.translate('取消'),
          minLength: 1,
        })
      : null
    if (encryptedBackup && password === null) return
    const backupPassword = password?.trim() || ''
    if (encryptedBackup && !backupPassword) {
      backupError.value = options.translate('加密备份密码不能为空')
      return
    }
    try {
      const response = await options.api<Settings>('/api/settings/backup/import', {
        method: 'POST',
        body: JSON.stringify(await options.encryptPayload('backup', {
          backup,
          password: backupPassword || null,
        })),
      })
      options.onImported(response)
      options.showNotice(options.translate('配置文件已还原，反代监听器已差量同步'))
    } catch (error) {
      backupError.value = error instanceof Error ? error.message : String(error)
    }
  }

  function downloadTextFile(content: string, filename: string) {
    const blob = new Blob([content], { type: 'application/json;charset=utf-8' })
    const url = URL.createObjectURL(blob)
    const link = document.createElement('a')
    link.href = url
    link.download = filename
    document.body.appendChild(link)
    link.click()
    link.remove()
    URL.revokeObjectURL(url)
  }

  function isEncryptedBackup(backup: string) {
    try {
      const value = JSON.parse(backup) as Record<string, unknown> | null
      return value !== null
        && typeof value === 'object'
        && Object.prototype.hasOwnProperty.call(value, 'cipher')
    } catch {
      return false
    }
  }

  function backupFileName() {
    const timestamp = new Date()
      .toISOString()
      .replace(/\.\d{3}Z$/, '')
      .replace(/[-:T]/g, '')
    return `embypanel-config-${timestamp}.json`
  }

  return {
    backupError,
    backupBusy,
    backupFileInput,
    exportBackup,
    importBackup,
    handleBackupFileSelected,
  }
}
