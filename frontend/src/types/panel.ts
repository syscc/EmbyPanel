import type * as forge from 'node-forge'

export type Settings = {
  emby_host: string
  emby_api_key: string
  servers: EmbyServerConfig[]
  openlist_addr: string | null
  openlist_token: string | null
  port: number
  cache_ttl_seconds: number
  cache_max_capacity: number
  cache_enabled: boolean
  cache_domain_filter_mode: 'off' | 'whitelist' | 'blacklist'
  cache_domain_whitelist: string
  enable_internal_redirect: boolean
  internal_redirect_timeout_seconds: number
  strm_url_mappings: string
  strm_url_mapping_enabled: boolean
  connectivity_check_enabled: boolean
  connectivity_check_interval_seconds: number
  connectivity_check_timeout_seconds: number
  connectivity_auto_restart_seconds: number
}

export type RealIpMode = 'auto' | 'header' | 'header_list' | 'xff_last' | 'xff_second_last' | 'xff_third_last'

export type EmbyServerConfig = {
  id: string
  name: string
  emby_host: string
  emby_api_key: string
  port: number
  enabled: boolean
  block_web_ui: boolean
  real_ip_mode: RealIpMode
  real_ip_header: string
  trusted_proxy_cidrs: string
}

export type PublicKeyResponse = {
  algorithm: string
  public_key_pem: string
}

export type AppInfo = {
  name: string
  version: string
  project_url: string
  ui_path: string
}

export type Profile = {
  username: string
}

export type PlaybackSession = {
  server_id: string
  server_name: string
  id: string
  item_id: string
  series_id: string | null
  media_source_id: string | null
  user_name: string
  client: string
  device_name: string
  user_agent: string
  playback_ip: string | null
  ip_location?: IpLocation
  item_name: string
  series_name: string | null
  item_type: string | null
  season_number: number | null
  episode_number: number | null
  position_ticks: number | null
  runtime_ticks: number | null
  percent: number | null
  play_method: string | null
  playback_mode: 'direct_link' | 'server_proxy' | 'transcode' | 'emby_direct_play' | 'emby_direct_stream' | 'unknown'
  transcoding: boolean
}

export type MediaOverview = {
  server_id: string
  movie_count: number
  series_count: number
  episode_count: number
  user_count: number
  server_name: string
  version: string
  operating_system: string
  library_count: number
}

export type MediaOverviewTotals = {
  movie_count: number
  series_count: number
  episode_count: number
  user_count: number
  library_count: number
}

export type ServerHealth = {
  uptime_seconds: number
  cpu_percent: number
  cpu_name: string
  cpu_cores: number
  memory_used_bytes: number
  memory_total_bytes: number
  memory_percent: number
  disk_used_bytes: number
  disk_total_bytes: number
  disk_percent: number
}

export type DetailedHealth = {
  status: string
  name: string
  version: string
  database: string
  proxy_count: number
}

export type ProxyStatus = {
  server_id: string
  server_name: string
  enabled: boolean
  port: number
  listening: boolean
  started_at_ms: number | null
  last_request_ms: number | null
  last_error: string | null
}

export type ConnectivityCheckStatus = {
  server_id: string
  server_name: string
  port: number
  enabled: boolean
  ok: boolean
  emby_ok: boolean
  openlist_ok: boolean | null
  proxy_ok: boolean
  checked_at_ms: number
  duration_ms: number
  failed_since_ms: number | null
  auto_restarted_at_ms: number | null
  last_error: string | null
}

export type RequestStatsDaily = {
  date: string
  server_id: string
  server_name: string
  port: number
  requests: number
  redirects: number
  cache_hits: number
  blocks: number
  errors: number
  updated_at_ms: number
}

export type IpLocation = {
  country_name: string
  region_name: string
  city_name: string
  district_name: string
  isp_domain: string
}

export type ProxyRequestDetail = {
  id: number
  event_type?: 'request' | 'block' | 'unblock'
  timestamp_ms: number
  server_id: string
  server_name: string
  port: number
  method: string
  path: string
  path_type: string
  status_code: number
  outcome: string
  duration_ms: number
  playback_user: string
  playback_ip: string
  ip_location?: IpLocation
  cache_hit: boolean
  blocked: boolean
  detail: string
}

export type UpdateCheck = {
  current_version: string
  latest_version: string
  release_url: string
  has_update: boolean
  checked_at_ms: number
}

export type ClientRuleRecord = {
  id: string
  client_name: string
  device_name: string
  user_name: string
  user_agent: string
  source: 'auto' | 'manual'
  enabled: boolean
  created_at: string
  updated_at: string
  note: string
}

export type PlaybackRateBlockRecord = {
  id: string
  server_id: string
  server_name: string
  action: 'block_ip' | 'block_user' | 'disable_user' | 'mixed'
  ip: string
  ip_location?: IpLocation
  user_name: string
  blocked_until: string
  created_at: string
  enabled: boolean
  note: string
}

export type PlaybackRateWindowStatus = {
  server_id: string
  block_id?: string
  block_action?: string
  user_name: string
  ip: string
  ip_location?: IpLocation
  current_count: number
  threshold: number
  remaining: number
  window_seconds: number
  reset_at: string
  blocked: boolean
}

export type WebhookNotifyConfig = {
  id: string
  enabled: boolean
  name: string
  url: string
  secret: string
}

export type ClientControlConfig = {
  enabled: boolean
  notify_enabled: boolean
  playback_rate_limit_enabled: boolean
  playback_rate_limit_window_seconds: number
  playback_rate_limit_max_requests: number
  playback_rate_limit_block_seconds: number
  playback_rate_limit_action: 'block_ip' | 'disable_user' | 'mixed'
  concurrent_playback_limit_enabled: boolean
  concurrent_playback_limit_max: number
  rate_limit_blocks: PlaybackRateBlockRecord[]
  webhook?: WebhookNotifyConfig
  webhooks: WebhookNotifyConfig[]
  records: ClientRuleRecord[]
}

export type ActivityLogEntry = {
  id: number
  timestamp_ms: number
  kind: 'playback' | 'general'
  level: 'success' | 'info' | 'warn' | 'error'
  server_id: string | null
  server_name: string
  playback_user: string | null
  playback_ip: string | null
  ip_location?: IpLocation
  message: string
  detail: string
}

export type AuditLogEntry = {
  id: number
  timestamp_ms: number
  admin_user_id: number | null
  admin_username: string
  action: string
  summary: string
  result: string
}

export type ValidationResult = {
  scope: string
  ok: boolean
  message: string
  detail: string
}

export type ValidationResponse = {
  ok: boolean
  results: ValidationResult[]
}

export type SystemLogConfig = {
  debug_mode: boolean
  level: 'debug' | 'info' | 'warning' | 'error' | 'critical'
  max_size_mb: number
  max_backups: number
  format: string
}

export type AuthMode = 'loading' | 'setup' | 'login' | 'app'
export type Page = 'home' | 'server' | 'clients' | 'notifications' | 'backup' | 'logs' | 'account' | 'users'
export type ClientStatusFilter = 'all' | 'blocked' | 'allowed'
export type LogKindFilter = 'all' | 'playback' | 'general'
export type LogViewFilter = 'playback' | 'blocked' | 'proxy' | 'general'
export type Locale = 'zh-CN' | 'en-US'
export type EncryptionPublicKey =
  | { kind: 'webcrypto'; key: CryptoKey }
  | { kind: 'forge'; key: forge.pki.rsa.PublicKey }

export type UserPolicyRecord = {
  server_id: string
  user_id: string
  user_name: string
  rate_limit_enabled: boolean
  rate_limit_window_seconds: number
  rate_limit_max_requests: number
  rate_limit_block_seconds: number
  rate_limit_action: 'block_ip' | 'block_user' | 'disable_user' | 'mixed' | string
  concurrent_playback_limit_enabled: boolean
  concurrent_playback_limit_max: number
}

export type UserAccessSchedule = {
  day_of_week: 'Sunday' | 'Monday' | 'Tuesday' | 'Wednesday' | 'Thursday' | 'Friday' | 'Saturday' | 'Everyday' | 'Weekday' | 'Weekend'
  start_hour: number
  end_hour: number
}

export type UserPolicyInput = {
  is_administrator?: boolean
  is_hidden?: boolean
  is_hidden_remotely?: boolean
  is_hidden_from_unused_devices?: boolean
  is_disabled?: boolean
  max_parental_rating_enabled?: boolean
  max_parental_rating?: number
  allow_tag_or_rating?: boolean
  blocked_tags?: string[]
  is_tag_blocking_mode_inclusive?: boolean
  include_tags?: string[]
  enable_user_preference_access?: boolean
  access_schedules?: UserAccessSchedule[]
  block_unrated_items?: string[]
  enable_remote_control_of_other_users?: boolean
  enable_shared_device_control?: boolean
  enable_remote_access?: boolean
  enable_live_tv_management?: boolean
  enable_live_tv_access?: boolean
  enable_media_playback?: boolean
  enable_audio_playback_transcoding?: boolean
  enable_video_playback_transcoding?: boolean
  auto_remote_quality?: number
  enable_playback_remuxing?: boolean
  enable_content_deletion?: boolean
  restricted_features?: string[]
  enable_content_deletion_from_folders?: string[]
  enable_content_downloading?: boolean
  enable_subtitle_downloading?: boolean
  enable_subtitle_management?: boolean
  enable_sync_transcoding?: boolean
  enable_media_conversion?: boolean
  enabled_channels?: string[]
  enable_all_channels?: boolean
  enable_all_folders?: boolean
  enabled_folders?: string[]
  enable_public_sharing?: boolean
  remote_client_bitrate_limit?: number
  excluded_sub_folders?: string[]
  enable_all_devices?: boolean
  enabled_devices?: string[]
  simultaneous_stream_limit?: number
  allow_camera_upload?: boolean
  allow_sharing_personal_items?: boolean
  rate_limit_enabled?: boolean
  rate_limit_window_seconds?: number
  rate_limit_max_requests?: number
  rate_limit_block_seconds?: number
  rate_limit_action?: 'block_ip' | 'block_user' | 'disable_user' | 'mixed' | string
  concurrent_playback_limit_enabled?: boolean
  concurrent_playback_limit_max?: number
}

export type UserPolicyDraft = {
  is_administrator: boolean
  is_hidden: boolean
  is_hidden_remotely: boolean
  is_hidden_from_unused_devices: boolean
  is_disabled: boolean
  max_parental_rating_enabled: boolean
  max_parental_rating: number
  allow_tag_or_rating: boolean
  blocked_tags: string[]
  is_tag_blocking_mode_inclusive: boolean
  include_tags: string[]
  enable_user_preference_access: boolean
  access_schedules: UserAccessSchedule[]
  block_unrated_items: string[]
  enable_remote_control_of_other_users: boolean
  enable_shared_device_control: boolean
  enable_remote_access: boolean
  enable_live_tv_management: boolean
  enable_live_tv_access: boolean
  enable_media_playback: boolean
  enable_audio_playback_transcoding: boolean
  enable_video_playback_transcoding: boolean
  auto_remote_quality: number
  enable_playback_remuxing: boolean
  enable_content_deletion: boolean
  restricted_features: string[]
  enable_content_deletion_from_folders: string[]
  enable_content_downloading: boolean
  enable_subtitle_downloading: boolean
  enable_subtitle_management: boolean
  enable_sync_transcoding: boolean
  enable_media_conversion: boolean
  enabled_channels: string[]
  enable_all_channels: boolean
  enable_all_folders: boolean
  enabled_folders: string[]
  enable_public_sharing: boolean
  remote_client_bitrate_limit: number
  excluded_sub_folders: string[]
  enable_all_devices: boolean
  enabled_devices: string[]
  simultaneous_stream_limit: number
  allow_camera_upload: boolean
  allow_sharing_personal_items: boolean
  rate_limit_enabled: boolean
  rate_limit_window_seconds: number
  rate_limit_max_requests: number
  rate_limit_block_seconds: number
  rate_limit_action: 'block_ip' | 'block_user' | 'disable_user' | 'mixed' | string
  concurrent_playback_limit_enabled: boolean
  concurrent_playback_limit_max: number
}

export type UserTemplate = {
  id: string
  server_id: string
  name: string
  policy: UserPolicyInput
}

export type UserSummary = {
  server_id: string
  server_name: string
  user_id: string
  name: string
  is_administrator: boolean
  is_disabled: boolean
  enable_remote_access: boolean
  enable_media_playback: boolean
  enable_all_folders: boolean
  enabled_folders: string[]
  available_folders: Array<{ id: string; name: string }>
  enable_all_devices: boolean
  enabled_devices: string[]
  available_devices: Array<{ id: string; name: string }>
  simultaneous_stream_limit: number | null
  last_activity: string | null
  active_sessions: number
  devices: string[]
  policy: UserPolicyInput
  user_policy: UserPolicyRecord
}

export type UsersResponse = {
  users: UserSummary[]
  servers?: Array<{ id: string; name: string }>
  server_errors: Array<{ server_id: string; server_name: string; error: string }>
}

export type UserTemplatesResponse = {
  templates: UserTemplate[]
}
