import type { Locale, ProxyRequestDetail } from '@/types/panel'

export function isHttpUrl(value: string) {
  const trimmed = value.trim()
  return trimmed.startsWith('http://') || trimmed.startsWith('https://')
}

export function emptyToNull(value: string | null) {
  const trimmed = value?.trim() ?? ''
  return trimmed || null
}

export function formatTicks(ticks: number | null) {
  if (!ticks || ticks < 0) return '--:--'
  const seconds = Math.floor(ticks / 10_000_000)
  const minutes = Math.floor(seconds / 60)
  const hours = Math.floor(minutes / 60)
  const mm = String(minutes % 60).padStart(2, '0')
  const ss = String(seconds % 60).padStart(2, '0')
  return hours > 0 ? `${hours}:${mm}:${ss}` : `${minutes}:${ss}`
}

export function formatBytes(bytes: number | undefined) {
  if (!bytes) return '--'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value >= 10 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`
}

export function formatUptime(seconds: number | undefined, locale: Locale) {
  if (!seconds) return '--'
  const days = Math.floor(seconds / 86400)
  const hours = Math.floor((seconds % 86400) / 3600)
  if (locale === 'en-US') return days > 0 ? `${days}d ${hours}h` : `${hours}h`
  return days > 0 ? `${days}天${hours}小时` : `${hours}小时`
}

export function formatTimestamp(value: string, locale: Locale) {
  const date = parseUnixTimestamp(value)
  return date ? date.toLocaleString(locale) : '--'
}

export function formatLogTime(value: number, locale: Locale) {
  if (!Number.isFinite(value) || value <= 0) return '--'
  return new Date(value).toLocaleString(locale)
}

export function formatTimestampMs(value: number | null | undefined, locale: Locale) {
  if (!value || !Number.isFinite(value)) return '--'
  return new Date(value).toLocaleString(locale)
}

export function requestOutcomeClass(row: ProxyRequestDetail) {
  if (row.event_type === 'unblock') return 'ok'
  if (row.blocked) return 'blocked'
  if (row.cache_hit) return 'cache'
  if (row.status_code >= 500) return 'error'
  if (row.status_code >= 400) return 'warn'
  if (row.status_code >= 300) return 'redirect'
  return 'ok'
}

function parseUnixTimestamp(value: string) {
  const timestamp = Number(value)
  if (!Number.isFinite(timestamp) || timestamp <= 0) return null
  return new Date(timestamp * 1000)
}
