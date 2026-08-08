<script setup lang="ts">
import { ChevronRight, Plus, Trash2 } from '@lucide/vue'
import { computed } from 'vue'
import CheckboxField from '@/components/ui/CheckboxField.vue'
import { usePanelContext } from '@/composables/panel-context'
import type { UserAccessSchedule, UserPolicyDraft } from '@/types/panel'

const props = withDefaults(defineProps<{
  draft: UserPolicyDraft
  folders?: Array<{ id: string; name: string }>
  devices?: Array<{ id: string; name: string }>
  showPanelLimits?: boolean
}>(), {
  folders: () => [],
  devices: () => [],
  showPanelLimits: true,
})

const { t } = usePanelContext()
const draft = props.draft

const dayOptions: Array<{ value: UserAccessSchedule['day_of_week']; label: string }> = [
  { value: 'Everyday', label: '每天' },
  { value: 'Weekday', label: '工作日' },
  { value: 'Weekend', label: '周末' },
  { value: 'Monday', label: '星期一' },
  { value: 'Tuesday', label: '星期二' },
  { value: 'Wednesday', label: '星期三' },
  { value: 'Thursday', label: '星期四' },
  { value: 'Friday', label: '星期五' },
  { value: 'Saturday', label: '星期六' },
  { value: 'Sunday', label: '星期日' },
]

const unratedOptions = [
  ['Movie', '电影'],
  ['Trailer', '预告片'],
  ['Series', '剧集'],
  ['Music', '音乐'],
  ['Game', '游戏'],
  ['Book', '图书'],
  ['LiveTvChannel', '直播频道'],
  ['LiveTvProgram', '直播节目'],
  ['ChannelContent', '频道内容'],
  ['Other', '其他'],
] as const

type TextListField =
  | 'blocked_tags'
  | 'include_tags'
  | 'restricted_features'
  | 'enabled_channels'
  | 'excluded_sub_folders'

const folderOptions = computed(() => accessOptions(props.folders, draft.enabled_folders))
const deviceOptions = computed(() => accessOptions(props.devices, draft.enabled_devices))
const deletionFolderOptions = computed(() =>
  accessOptions(props.folders, draft.enable_content_deletion_from_folders),
)

function accessOptions(
  available: Array<{ id: string; name: string }>,
  selected: string[],
) {
  const options = new Map(available.map((item) => [item.id, item.name]))
  for (const id of selected) if (!options.has(id)) options.set(id, id)
  return Array.from(options, ([id, name]) => ({ id, name }))
}

function setSelection(
  field: 'enabled_folders' | 'enabled_devices' | 'enable_content_deletion_from_folders',
  id: string,
  checked: boolean,
) {
  const current = draft[field]
  draft[field] = checked
    ? Array.from(new Set([...current, id]))
    : current.filter((value) => value !== id)
}

function setUnrated(value: string, checked: boolean) {
  draft.block_unrated_items = checked
    ? Array.from(new Set([...draft.block_unrated_items, value]))
    : draft.block_unrated_items.filter((item) => item !== value)
}

function listText(field: TextListField) {
  return draft[field].join('\n')
}

function setList(field: TextListField, event: Event) {
  draft[field] = Array.from(new Set(
    (event.target as HTMLTextAreaElement).value
      .split(/[\n,]/)
      .map((value) => value.trim())
      .filter(Boolean),
  ))
}

function addSchedule() {
  draft.access_schedules.push({ day_of_week: 'Everyday', start_hour: 0, end_hour: 24 })
}

function removeSchedule(index: number) {
  draft.access_schedules.splice(index, 1)
}
</script>

<template>
  <div class="user-policy-fields">
    <details class="user-policy-section" open>
      <summary><ChevronRight :size="16" aria-hidden="true" />{{ t('账户与登录显示') }}</summary>
      <div class="user-policy-toggle-grid">
        <CheckboxField v-model="draft.is_administrator" :label="t('管理员权限')" />
        <CheckboxField v-model="draft.is_disabled" :label="t('禁用用户')" />
        <CheckboxField v-model="draft.is_hidden" :label="t('在本地网络的登录界面中隐藏此用户')" />
        <CheckboxField v-model="draft.is_hidden_remotely" :label="t('在远程连接的登录界面中隐藏此用户')" />
        <CheckboxField v-model="draft.is_hidden_from_unused_devices" :label="t('在从未登录过设备的登录页面中隐藏此用户')" />
        <CheckboxField v-model="draft.enable_user_preference_access" :label="t('允许该用户更改其头像和密码')" />
      </div>
    </details>

    <details class="user-policy-section" open>
      <summary><ChevronRight :size="16" aria-hidden="true" />{{ t('访问范围与设备') }}</summary>
      <div class="user-policy-toggle-grid">
        <CheckboxField v-model="draft.enable_remote_access" :label="t('允许远程访问')" />
        <CheckboxField v-model="draft.enable_remote_control_of_other_users" :label="t('允许远程控制其他用户')" />
        <CheckboxField v-model="draft.enable_shared_device_control" :label="t('允许共享设备控制')" />
        <CheckboxField v-model="draft.enable_all_folders" :label="t('允许全部媒体库')" />
        <CheckboxField v-model="draft.enable_all_devices" :label="t('允许全部设备')" />
        <CheckboxField v-model="draft.enable_all_channels" :label="t('允许全部频道')" />
      </div>
      <div class="user-policy-field-grid">
        <label>
          <span>{{ t('媒体库白名单') }}</span>
          <div class="user-policy-access-list" :class="{ disabled: draft.enable_all_folders }">
            <CheckboxField
              v-for="folder in folderOptions"
              :key="folder.id"
              :model-value="draft.enabled_folders.includes(folder.id)"
              :label="folder.name"
              :disabled="draft.enable_all_folders"
              @update:model-value="setSelection('enabled_folders', folder.id, $event)"
            />
            <span v-if="!folderOptions.length" class="muted">{{ t('暂无') }}</span>
          </div>
        </label>
        <label>
          <span>{{ t('设备白名单') }}</span>
          <div class="user-policy-access-list" :class="{ disabled: draft.enable_all_devices }">
            <CheckboxField
              v-for="device in deviceOptions"
              :key="device.id"
              :model-value="draft.enabled_devices.includes(device.id)"
              :label="device.name"
              :disabled="draft.enable_all_devices"
              @update:model-value="setSelection('enabled_devices', device.id, $event)"
            />
            <span v-if="!deviceOptions.length" class="muted">{{ t('暂无') }}</span>
          </div>
        </label>
        <label><span>{{ t('频道白名单 ID') }}</span><textarea :value="listText('enabled_channels')" :disabled="draft.enable_all_channels" rows="3" @input="setList('enabled_channels', $event)" /></label>
        <label><span>{{ t('排除的子目录 ID') }}</span><textarea :value="listText('excluded_sub_folders')" rows="3" @input="setList('excluded_sub_folders', $event)" /></label>
      </div>
    </details>

    <details class="user-policy-section">
      <summary><ChevronRight :size="16" aria-hidden="true" />{{ t('播放、转码与下载') }}</summary>
      <div class="user-policy-toggle-grid">
        <CheckboxField v-model="draft.enable_media_playback" :label="t('允许媒体播放')" />
        <CheckboxField v-model="draft.enable_audio_playback_transcoding" :label="t('允许音频转码')" />
        <CheckboxField v-model="draft.enable_video_playback_transcoding" :label="t('允许视频转码')" />
        <CheckboxField v-model="draft.enable_playback_remuxing" :label="t('允许封装转换')" />
        <CheckboxField v-model="draft.enable_content_downloading" :label="t('允许媒体下载')" />
        <CheckboxField v-model="draft.enable_subtitle_downloading" :label="t('允许字幕下载')" />
        <CheckboxField v-model="draft.enable_subtitle_management" :label="t('允许字幕管理')" />
        <CheckboxField v-model="draft.enable_sync_transcoding" :label="t('允许同步转码')" />
        <CheckboxField v-model="draft.enable_media_conversion" :label="t('允许媒体转换')" />
        <CheckboxField v-model="draft.enable_live_tv_access" :label="t('允许直播电视访问')" />
        <CheckboxField v-model="draft.enable_live_tv_management" :label="t('允许直播电视管理')" />
      </div>
      <div class="user-policy-field-grid compact">
        <label><span>{{ t('Emby 同时播放上限') }}</span><input v-model.number="draft.simultaneous_stream_limit" type="number" min="0" max="64" /></label>
        <label><span>{{ t('自动远程画质值') }}</span><input v-model.number="draft.auto_remote_quality" type="number" min="0" max="1000000000" /></label>
        <label><span>{{ t('远程客户端码率上限') }}</span><input v-model.number="draft.remote_client_bitrate_limit" type="number" min="0" max="4294967295" /></label>
      </div>
    </details>

    <details class="user-policy-section">
      <summary><ChevronRight :size="16" aria-hidden="true" />{{ t('内容管理与共享') }}</summary>
      <div class="user-policy-toggle-grid">
        <CheckboxField v-model="draft.enable_content_deletion" :label="t('允许删除媒体')" />
        <CheckboxField v-model="draft.enable_public_sharing" :label="t('允许公开分享')" />
        <CheckboxField v-model="draft.allow_camera_upload" :label="t('允许相机上传')" />
        <CheckboxField v-model="draft.allow_sharing_personal_items" :label="t('允许分享个人项目')" />
      </div>
      <label class="user-policy-full-field">
        <span>{{ t('允许删除媒体的文件夹') }}</span>
        <div class="user-policy-access-list" :class="{ disabled: !draft.enable_content_deletion }">
          <CheckboxField
            v-for="folder in deletionFolderOptions"
            :key="folder.id"
            :model-value="draft.enable_content_deletion_from_folders.includes(folder.id)"
            :label="folder.name"
            :disabled="!draft.enable_content_deletion"
            @update:model-value="setSelection('enable_content_deletion_from_folders', folder.id, $event)"
          />
          <span v-if="!deletionFolderOptions.length" class="muted">{{ t('暂无') }}</span>
        </div>
      </label>
    </details>

    <details class="user-policy-section">
      <summary><ChevronRight :size="16" aria-hidden="true" />{{ t('家长控制与访问时段') }}</summary>
      <div class="user-policy-toggle-grid">
        <CheckboxField v-model="draft.max_parental_rating_enabled" :label="t('启用最高家长分级')" />
        <CheckboxField v-model="draft.allow_tag_or_rating" :label="t('标签或分级任一匹配即可访问')" />
        <CheckboxField v-model="draft.is_tag_blocking_mode_inclusive" :label="t('仅允许包含指定标签的内容')" />
      </div>
      <div class="user-policy-field-grid compact">
        <label><span>{{ t('最高家长分级值') }}</span><input v-model.number="draft.max_parental_rating" type="number" min="0" max="10000" :disabled="!draft.max_parental_rating_enabled" /></label>
        <label><span>{{ t('屏蔽标签') }}</span><textarea :value="listText('blocked_tags')" rows="3" @input="setList('blocked_tags', $event)" /></label>
        <label><span>{{ t('允许标签') }}</span><textarea :value="listText('include_tags')" rows="3" @input="setList('include_tags', $event)" /></label>
        <label><span>{{ t('受限功能标识') }}</span><textarea :value="listText('restricted_features')" rows="3" @input="setList('restricted_features', $event)" /></label>
      </div>
      <div class="user-policy-subsection">
        <span>{{ t('屏蔽未分级内容') }}</span>
        <div class="user-policy-toggle-grid unrated">
          <CheckboxField
            v-for="option in unratedOptions"
            :key="option[0]"
            :model-value="draft.block_unrated_items.includes(option[0])"
            :label="t(option[1])"
            @update:model-value="setUnrated(option[0], $event)"
          />
        </div>
      </div>
      <div class="user-policy-subsection">
        <div class="user-policy-subsection-head">
          <span>{{ t('访问时段') }}</span>
          <button class="secondary" type="button" @click="addSchedule"><Plus :size="14" />{{ t('添加时段') }}</button>
        </div>
        <div v-for="(schedule, index) in draft.access_schedules" :key="index" class="user-policy-schedule">
          <select v-model="schedule.day_of_week">
            <option v-for="day in dayOptions" :key="day.value" :value="day.value">{{ t(day.label) }}</option>
          </select>
          <input v-model.number="schedule.start_hour" type="number" min="0" max="23.99" step="0.5" :aria-label="t('开始时间')" />
          <span>–</span>
          <input v-model.number="schedule.end_hour" type="number" min="0.01" max="24" step="0.5" :aria-label="t('结束时间')" />
          <button class="danger-button icon-button" type="button" :aria-label="t('删除时段')" @click="removeSchedule(index)"><Trash2 :size="14" /></button>
        </div>
        <span v-if="!draft.access_schedules.length" class="muted">{{ t('未限制访问时段') }}</span>
      </div>
    </details>

    <details v-if="showPanelLimits" class="user-policy-section">
      <summary><ChevronRight :size="16" aria-hidden="true" />{{ t('面板用户限流') }}</summary>
      <div class="user-policy-toggle-grid">
        <CheckboxField v-model="draft.concurrent_playback_limit_enabled" :label="t('启用用户同时播放限制')" />
        <CheckboxField v-model="draft.rate_limit_enabled" :label="t('启用用户播放频率限制')" />
      </div>
      <div class="user-policy-field-grid compact">
        <label><span>{{ t('允许同时播放数') }}</span><input v-model.number="draft.concurrent_playback_limit_max" type="number" min="1" max="64" :disabled="!draft.concurrent_playback_limit_enabled" /></label>
        <label><span>{{ t('频率窗口（秒）') }}</span><input v-model.number="draft.rate_limit_window_seconds" type="number" min="1" max="86400" :disabled="!draft.rate_limit_enabled" /></label>
        <label><span>{{ t('窗口内最大次数') }}</span><input v-model.number="draft.rate_limit_max_requests" type="number" min="1" max="10000" :disabled="!draft.rate_limit_enabled" /></label>
        <label><span>{{ t('封禁时长（秒）') }}</span><input v-model.number="draft.rate_limit_block_seconds" type="number" min="1" max="86400" :disabled="!draft.rate_limit_enabled" /></label>
        <label><span>{{ t('限流动作') }}</span><select v-model="draft.rate_limit_action" :disabled="!draft.rate_limit_enabled"><option value="block_ip">{{ t('屏蔽 IP') }}</option><option value="block_user">{{ t('封禁用户') }}</option><option value="disable_user">{{ t('禁用用户') }}</option><option value="mixed">{{ t('混合处理') }}</option></select></label>
      </div>
    </details>
  </div>
</template>

<style scoped>
.user-policy-fields { display: grid; gap: 10px; }
.user-policy-section { min-width: 0; overflow: hidden; padding: 0 14px 14px; border: 1px solid var(--border); border-radius: 7px; background: var(--panel-bg); }
.user-policy-section > summary { display: flex; align-items: center; gap: 8px; min-height: 44px; margin: 0 -14px; padding: 10px 14px; cursor: pointer; list-style: none; color: var(--text); font-size: 13px; font-weight: 700; }
.user-policy-section > summary::-webkit-details-marker { display: none; }
.user-policy-section > summary:hover { background: var(--subtle-bg); }
.user-policy-section > summary:focus-visible { outline: 2px solid color-mix(in srgb, var(--focus-ring) 72%, transparent); outline-offset: -2px; }
.user-policy-section > summary svg { flex: 0 0 auto; color: var(--muted); transition: transform 160ms ease; }
.user-policy-section[open] > summary { margin-bottom: 14px; border-bottom: 1px solid var(--border-soft); background: var(--subtle-bg); }
.user-policy-section[open] > summary svg { transform: rotate(90deg); }
.user-policy-toggle-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 6px 16px; }
.user-policy-field-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; margin-top: 12px; }
.user-policy-field-grid.compact { grid-template-columns: repeat(3, minmax(0, 1fr)); }
.user-policy-field-grid > label, .user-policy-full-field { display: grid; gap: 6px; }
.user-policy-field-grid > label > span, .user-policy-full-field > span, .user-policy-subsection > span { color: var(--muted); font-size: 12px; }
.user-policy-field-grid textarea { min-height: 74px; resize: vertical; }
.user-policy-access-list { display: grid; gap: 4px; max-height: 138px; overflow-y: auto; border: 1px solid var(--border); border-radius: 6px; padding: 8px 10px; background: var(--subtle-bg); }
.user-policy-access-list.disabled { opacity: .55; }
.user-policy-full-field { margin-top: 12px; }
.user-policy-subsection { display: grid; gap: 9px; margin-top: 13px; }
.user-policy-subsection-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; font-size: 12px; color: var(--muted); }
.user-policy-schedule { display: grid; grid-template-columns: minmax(130px, 1fr) 92px auto 92px 34px; align-items: center; gap: 8px; }
.user-policy-schedule .icon-button { width: 34px; height: 34px; }
.user-policy-fields :deep(.check) { min-height: 34px; }
.user-policy-access-list :deep(.check) { min-height: 28px; }
.unrated { grid-template-columns: repeat(3, minmax(0, 1fr)); }
@media (max-width: 720px) {
  .user-policy-toggle-grid, .user-policy-field-grid, .user-policy-field-grid.compact, .unrated { grid-template-columns: 1fr; }
  .user-policy-schedule { grid-template-columns: minmax(0, 1fr) 72px auto 72px 34px; }
}
@media (max-width: 480px) {
  .user-policy-section { padding-right: 12px; padding-left: 12px; }
  .user-policy-section > summary { margin-right: -12px; margin-left: -12px; padding-right: 12px; padding-left: 12px; }
  .user-policy-schedule { display: flex; flex-wrap: wrap; gap: 7px; }
  .user-policy-schedule select { flex: 1 1 100%; min-width: 0; }
  .user-policy-schedule input { flex: 1 1 72px; min-width: 0; }
  .user-policy-schedule span { align-self: center; }
}
</style>
