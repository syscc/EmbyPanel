import type { UserPolicyDraft, UserPolicyInput, UserSummary, UserTemplate } from '@/types/panel'

const policyListFields = [
  'blocked_tags',
  'include_tags',
  'block_unrated_items',
  'restricted_features',
  'enable_content_deletion_from_folders',
  'enabled_channels',
  'enabled_folders',
  'excluded_sub_folders',
  'enabled_devices',
] as const

type PolicyListField = typeof policyListFields[number]

export function defaultPolicyDraft(): UserPolicyDraft {
  return {
    is_administrator: false,
    is_hidden: true,
    is_hidden_remotely: true,
    is_hidden_from_unused_devices: true,
    is_disabled: false,
    max_parental_rating_enabled: false,
    max_parental_rating: 0,
    allow_tag_or_rating: false,
    blocked_tags: [],
    is_tag_blocking_mode_inclusive: false,
    include_tags: [],
    enable_user_preference_access: true,
    access_schedules: [],
    block_unrated_items: [],
    enable_remote_control_of_other_users: false,
    enable_shared_device_control: false,
    enable_remote_access: true,
    enable_live_tv_management: false,
    enable_live_tv_access: false,
    enable_media_playback: true,
    enable_audio_playback_transcoding: false,
    enable_video_playback_transcoding: false,
    auto_remote_quality: 0,
    enable_playback_remuxing: false,
    enable_content_deletion: false,
    restricted_features: [],
    enable_content_deletion_from_folders: [],
    enable_content_downloading: false,
    enable_subtitle_downloading: false,
    enable_subtitle_management: false,
    enable_sync_transcoding: false,
    enable_media_conversion: false,
    enabled_channels: [],
    enable_all_channels: true,
    enable_all_folders: true,
    enabled_folders: [],
    enable_public_sharing: false,
    remote_client_bitrate_limit: 0,
    excluded_sub_folders: [],
    enable_all_devices: true,
    enabled_devices: [],
    simultaneous_stream_limit: 0,
    allow_camera_upload: false,
    allow_sharing_personal_items: false,
    rate_limit_enabled: false,
    rate_limit_window_seconds: 60,
    rate_limit_max_requests: 20,
    rate_limit_block_seconds: 1800,
    rate_limit_action: 'block_ip',
    concurrent_playback_limit_enabled: false,
    concurrent_playback_limit_max: 3,
  }
}

function mergePolicyValues(target: UserPolicyDraft, source: UserPolicyInput) {
  for (const [key, value] of Object.entries(source) as Array<[
    keyof UserPolicyInput,
    UserPolicyInput[keyof UserPolicyInput],
  ]>) {
    if (value === undefined || value === null) continue
    if (key === 'access_schedules') {
      target.access_schedules = (value as NonNullable<UserPolicyInput['access_schedules']>)
        .map((item) => ({ ...item }))
    } else if ((policyListFields as readonly string[]).includes(key)) {
      target[key as PolicyListField] = [...(value as string[])] as never
    } else {
      target[key as keyof UserPolicyDraft] = value as never
    }
  }
}

export function copyPolicy(target: UserPolicyDraft, source: UserPolicyInput) {
  Object.assign(target, defaultPolicyDraft())
  mergePolicyValues(target, source)
}

export function policyPayload(source: UserPolicyDraft): UserPolicyInput {
  return {
    ...source,
    blocked_tags: [...source.blocked_tags],
    include_tags: [...source.include_tags],
    access_schedules: source.access_schedules.map((item) => ({ ...item })),
    block_unrated_items: [...source.block_unrated_items],
    restricted_features: [...source.restricted_features],
    enable_content_deletion_from_folders: [...source.enable_content_deletion_from_folders],
    enabled_channels: [...source.enabled_channels],
    enabled_folders: [...source.enabled_folders],
    excluded_sub_folders: [...source.excluded_sub_folders],
    enabled_devices: [...source.enabled_devices],
    max_parental_rating: Math.max(0, Number(source.max_parental_rating) || 0),
    auto_remote_quality: Math.max(0, Number(source.auto_remote_quality) || 0),
    remote_client_bitrate_limit: Math.max(0, Number(source.remote_client_bitrate_limit) || 0),
    simultaneous_stream_limit: Math.max(0, Number(source.simultaneous_stream_limit) || 0),
    rate_limit_window_seconds: Number(source.rate_limit_window_seconds) || 60,
    rate_limit_max_requests: Number(source.rate_limit_max_requests) || 20,
    rate_limit_block_seconds: Number(source.rate_limit_block_seconds) || 1800,
    concurrent_playback_limit_max: Number(source.concurrent_playback_limit_max) || 3,
  }
}

export function policyFromUser(user: UserSummary): UserPolicyInput {
  return {
    ...user.policy,
    is_administrator: user.is_administrator,
    is_disabled: user.is_disabled,
    enable_remote_access: user.enable_remote_access,
    enable_media_playback: user.enable_media_playback,
    enable_all_folders: user.enable_all_folders,
    enabled_folders: [...user.enabled_folders],
    enable_all_devices: user.enable_all_devices,
    enabled_devices: [...user.enabled_devices],
    simultaneous_stream_limit: user.simultaneous_stream_limit ?? 0,
    rate_limit_enabled: user.user_policy.rate_limit_enabled,
    rate_limit_window_seconds: user.user_policy.rate_limit_window_seconds,
    rate_limit_max_requests: user.user_policy.rate_limit_max_requests,
    rate_limit_block_seconds: user.user_policy.rate_limit_block_seconds,
    rate_limit_action: user.user_policy.rate_limit_action,
    concurrent_playback_limit_enabled: user.user_policy.concurrent_playback_limit_enabled,
    concurrent_playback_limit_max: user.user_policy.concurrent_playback_limit_max,
  }
}

export function applyTemplateTo(target: UserPolicyDraft, template: UserTemplate | null) {
  if (!template) return
  mergePolicyValues(target, template.policy)
}
