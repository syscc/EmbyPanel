<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import {
  Activity, CircleAlert, CircleCheck, Clock3, FileText, FolderOpen,
  HardDrive, LibraryBig, ListTodo, LoaderCircle, Pause, Play, Server,
} from "@lucide/vue";
import { usePanelContext } from "@/composables/panel-context";
import {
  useLibraryTasksController,
  type LibraryLocationStatus,
  type LibrarySummary,
  type LibraryTask,
} from "@/composables/useLibraryTasksController";
import RefreshButton from "@/components/ui/RefreshButton.vue";
import SettingsDialogShell from "@/components/ui/SettingsDialogShell.vue";

const panel = usePanelContext();
const {
  api,
  encryptPayload,
  t,
  showNotice,
  clearNotice,
  confirmAction,
  promptAction,
  formatLogTime,
} = panel;
const controller = useLibraryTasksController({
  api,
  servers: () => panel.serverOptions.value,
  encryptPayload,
  t,
  confirmAction,
  promptAction,
  showNotice,
  clearNotice,
});
const {
  tasks,
  libraries,
  selectedServer,
  serverOptions,
  serverName,
  failureDetail,
  loading,
  error,
  partialError,
  failureLoading,
  failureError,
  actionTaskId,
  startTask,
  cancelTask,
  validatePaths,
  viewTaskLogs,
  closeFailureDetail,
  refresh,
  dispose,
} = controller;

const dialogOpen = ref(false);
const selectedTask = ref<LibraryTask | null>(null);
const refreshing = ref(false);
const runningTaskCount = computed(() => tasks.value.filter((task) => task.state === "Running").length);
const idleTaskCount = computed(() => tasks.value.filter((task) => task.state === "Idle").length);
const failedTaskCount = computed(() => tasks.value.filter((task) => task.state.toLowerCase() === "failed" || Boolean(task.error)).length);

async function refreshPage() {
  if (refreshing.value || loading.value) return;
  refreshing.value = true;
  try {
    if (!await panel.refreshServerOptions()) return;
    await refresh();
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    refreshing.value = false;
  }
}

function taskKey(task: LibraryTask) {
  return `${task.server_id}:${task.id}`;
}

function unavailableLabel(capability: {
  available: boolean;
  unavailable_reason?: string;
}) {
  if (capability.available) return "";
  return capability.unavailable_reason === "emby_not_provided"
    ? t("Emby 未提供")
    : t("不可用");
}

function displayCapability(
  value: string | null,
  capability: { available: boolean; unavailable_reason?: string },
) {
  return value !== null && capability.available
    ? value
    : unavailableLabel(capability);
}

function libraryLocations(library: LibrarySummary) {
  if (library.locations?.length) return library.locations;
  return library.path !== null ? [library.path] : [];
}

function locationStatus(
  library: LibrarySummary,
  path: string,
): LibraryLocationStatus | undefined {
  return library.location_statuses?.find((entry) => entry.path === path);
}

function locationStatusLabel(status: LibraryLocationStatus | undefined) {
  if (!status || status.status === "unvalidated") return t("需要显式验证");
  return status.status === "valid" ? t("可用") : t("校验失败");
}

function locationStatusClass(status: LibraryLocationStatus | undefined) {
  if (!status || status.status === "unvalidated") return "is-neutral";
  return status.status === "valid" ? "is-success" : "is-danger";
}

const taskStateTranslationKeys: Record<string, string> = {
  Running: "运行中",
  Idle: "空闲",
  Stopping: "停止中",
  Completed: "已完成",
  Failed: "失败",
  completed: "已完成",
  failed: "失败",
};

function taskStateLabel(state: string) {
  return t(taskStateTranslationKeys[state] ?? "未知任务状态");
}

function taskStateClass(task: LibraryTask) {
  if (task.error || task.state.toLowerCase() === "failed") return "is-danger";
  if (task.state === "Running") return "is-running";
  if (task.state === "Stopping") return "is-warning";
  if (task.state.toLowerCase() === "completed") return "is-success";
  return "is-neutral";
}

function failureStatusLabel(status: string) {
  const translationKey = taskStateTranslationKeys[status];
  return translationKey ? t(translationKey) : status || t("未知任务状态");
}

function failureTime(value: number | null | undefined) {
  return value === null || value === undefined ? t("无") : formatLogTime(value);
}

function progressPercent(progress: number) {
  if (!Number.isFinite(progress)) return 0;
  return Math.max(0, Math.min(100, progress));
}

function progressWidth(progress: number) {
  return `${progressPercent(progress)}%`;
}

async function handleViewLogs(task: LibraryTask) {
  selectedTask.value = task;
  dialogOpen.value = true;
  await viewTaskLogs(task);
}

function onDetailOpenChange(open: boolean) {
  dialogOpen.value = open;
  if (!open) {
    closeFailureDetail();
    selectedTask.value = null;
  }
}

onMounted(() => {
  void refresh();
});

onUnmounted(() => {
  dispose();
});
</script>

<template>
  <div class="library-tasks-page" :aria-busy="loading || refreshing">
    <header class="library-page-head">
      <div class="library-page-heading">
        <span class="library-page-icon" aria-hidden="true"><LibraryBig :size="23" /></span>
        <div>
          <h2>{{ t("媒体库与任务") }}</h2>
          <p>{{ t("管理媒体库路径，掌握扫描任务动态。") }}</p>
        </div>
      </div>
      <div class="library-toolbar">
        <div class="library-server-select">
          <Server :size="15" aria-hidden="true" />
          <select v-model="selectedServer" :aria-label="t('服务器')">
            <option v-for="option in serverOptions" :key="option.value" :value="option.value">
              {{ option.label }}
            </option>
          </select>
        </div>
        <RefreshButton
          :label="t('刷新')"
          :loading-label="t('刷新中')"
          :action="refreshPage"
          :loading="loading || refreshing"
          :disabled="loading || refreshing"
        />
      </div>
    </header>

    <div v-if="partialError" class="library-notice is-warning" role="status" aria-live="polite">
      <CircleAlert :size="18" aria-hidden="true" /><span>{{ partialError }}</span>
    </div>
    <div v-if="error" class="library-notice is-danger" role="alert" aria-live="assertive">
      <CircleAlert :size="18" aria-hidden="true" /><span>{{ error }}</span>
    </div>

    <section class="library-metrics" :aria-label="t('任务概览')">
      <div class="library-metric">
        <span class="metric-icon" aria-hidden="true"><LibraryBig :size="19" /></span>
        <dl><dt>{{ t("媒体库") }}</dt><dd>{{ loading || error || partialError ? '—' : libraries.length }}</dd></dl>
      </div>
      <div class="library-metric">
        <span class="metric-icon is-running" aria-hidden="true"><Activity :size="19" /></span>
        <dl><dt>{{ t("运行中") }}</dt><dd>{{ loading || error || partialError ? '—' : runningTaskCount }}</dd></dl>
      </div>
      <div class="library-metric">
        <span class="metric-icon is-neutral" aria-hidden="true"><Clock3 :size="19" /></span>
        <dl><dt>{{ t("空闲") }}</dt><dd>{{ loading || error || partialError ? '—' : idleTaskCount }}</dd></dl>
      </div>
      <div class="library-metric">
        <span class="metric-icon" :class="failedTaskCount ? 'is-danger' : 'is-neutral'" aria-hidden="true"><CircleAlert :size="19" /></span>
        <dl><dt>{{ t("异常任务") }}</dt><dd>{{ loading || error || partialError ? '—' : failedTaskCount }}</dd></dl>
      </div>
    </section>

    <section class="library-section" aria-labelledby="library-overview-title">
      <div class="library-section-head">
        <div class="library-section-title">
          <FolderOpen :size="18" aria-hidden="true" />
          <h3 id="library-overview-title">{{ t("媒体库总览") }}</h3>
          <span class="library-count">{{ loading ? '—' : libraries.length }}</span>
        </div>
        <p>{{ t("路径检查仅在手动验证时执行") }}</p>
      </div>
      <div v-if="loading" class="library-grid" aria-hidden="true">
        <div v-for="index in 3" :key="index" class="library-skeleton">
          <div class="skeleton-line short"></div><div class="skeleton-line"></div><div class="skeleton-line"></div>
        </div>
      </div>
      <div v-else-if="!libraries.length" class="library-empty">
        <FolderOpen :size="28" aria-hidden="true" />
        <strong>{{ error || partialError ? t("暂无详情") : t("暂无媒体库") }}</strong>
        <p>{{ t("选择服务器或刷新以获取媒体库信息。") }}</p>
      </div>
      <div v-else class="library-grid">
        <article v-for="library in libraries" :key="`${library.server_id}-${library.name}`" class="library-card">
          <header class="library-card-head">
            <span class="library-folder-icon" aria-hidden="true"><FolderOpen :size="18" /></span>
            <div class="library-card-title">
              <h4>{{ library.name }}</h4>
              <p><Server :size="12" aria-hidden="true" />{{ serverName(library.server_id) }}</p>
            </div>
          </header>
          <ul v-if="libraryLocations(library).length" class="library-path-list" :aria-label="t('路径')">
            <li v-for="(path, index) in libraryLocations(library)" :key="`${path}-${index}`" class="library-path-item">
              <div class="library-path-content">
                <div class="library-path-line"><HardDrive :size="12" aria-hidden="true" /><code>{{ path }}</code></div>
                <span class="library-status" :class="locationStatusClass(locationStatus(library, path))" :title="locationStatusLabel(locationStatus(library, path))">
                  <span class="library-status-dot" aria-hidden="true"></span>
                  {{ !locationStatus(library, path) || locationStatus(library, path)?.status === 'unvalidated' ? t("未校验") : locationStatusLabel(locationStatus(library, path)) }}
                </span>
              </div>
              <button type="button" class="library-validate" :disabled="Boolean(actionTaskId)" :aria-label="`${t('验证路径')}: ${path}`" :title="`${t('验证路径')}: ${path}`" @click="validatePaths([path], library.server_id)">
                <CircleCheck :size="16" aria-hidden="true" />
              </button>
              <p v-if="locationStatus(library, path)?.error" class="library-error-text">
                {{ t("错误摘要") }}: {{ locationStatus(library, path)?.error }}
              </p>
            </li>
          </ul>
          <p v-else class="library-unavailable">{{ unavailableLabel(library.capabilities.path) }}</p>
        </article>
      </div>
    </section>

    <section class="library-section" aria-labelledby="library-tasks-title">
      <div class="library-section-head">
        <div class="library-section-title">
          <ListTodo :size="18" aria-hidden="true" />
          <h3 id="library-tasks-title">{{ t("扫描任务") }}</h3>
          <span class="library-count">{{ loading ? '—' : tasks.length }}</span>
        </div>
        <span v-if="runningTaskCount" class="library-status is-running">
          <span class="library-status-dot" aria-hidden="true"></span>{{ runningTaskCount }} {{ t("个任务运行中") }}
        </span>
      </div>
      <div v-if="loading" class="library-empty" role="status" aria-live="polite">
        <LoaderCircle :size="26" class="animate-spin" aria-hidden="true" />
        <strong>{{ t("加载中...") }}</strong>
      </div>
      <div v-else-if="!tasks.length" class="library-empty">
        <ListTodo :size="28" aria-hidden="true" />
        <strong>{{ error || partialError ? t("暂无详情") : t("暂无库扫描任务") }}</strong>
      </div>
      <ul v-else class="library-task-list" role="list">
        <li v-for="task in tasks" :key="taskKey(task)" class="library-task" :class="{ 'task-running': task.state === 'Running' }">
          <div class="library-task-main">
            <span class="library-task-icon" :class="taskStateClass(task)" aria-hidden="true">
              <Activity v-if="task.state === 'Running'" :size="19" />
              <CircleAlert v-else-if="task.error || task.state.toLowerCase() === 'failed'" :size="19" />
              <ListTodo v-else :size="19" />
            </span>
            <div class="library-task-identity">
              <div class="library-task-title">
                <h4>{{ task.name }}</h4>
                <span class="library-status" :class="taskStateClass(task)">
                  <span class="library-status-dot" aria-hidden="true"></span>{{ taskStateLabel(task.state) }}
                </span>
              </div>
              <p class="library-task-server"><Server :size="12" aria-hidden="true" />{{ serverName(task.server_id) }}</p>
            </div>
            <div class="library-task-actions">
              <button v-if="task.state === 'Idle'" type="button" class="secondary task-start" :disabled="Boolean(actionTaskId)" :aria-label="`${t('开始扫描')}: ${task.name}`" @click="startTask(task.id, task.server_id)">
                <LoaderCircle v-if="actionTaskId === taskKey(task)" :size="14" class="animate-spin" aria-hidden="true" />
                <Play v-else :size="14" aria-hidden="true" />{{ t("开始扫描") }}
              </button>
              <button v-else-if="task.state === 'Running'" type="button" class="danger-button" :disabled="Boolean(actionTaskId)" :aria-label="`${t('取消任务')}: ${task.name}`" @click="cancelTask(task.id, task.server_id)">
                <LoaderCircle v-if="actionTaskId === taskKey(task)" :size="14" class="animate-spin" aria-hidden="true" />
                <Pause v-else :size="14" aria-hidden="true" />{{ t("取消任务") }}
              </button>
              <button type="button" class="secondary task-log" :aria-label="`${t('日志')}: ${task.name}`" @click="handleViewLogs(task)">
                <FileText :size="14" aria-hidden="true" />{{ t("日志") }}
              </button>
            </div>
          </div>
          <div class="library-task-details">
            <div class="library-task-path"><HardDrive :size="13" aria-hidden="true" /><span>{{ t("路径") }}</span><code>{{ displayCapability(task.path, task.capabilities.path) }}</code></div>
            <div class="library-task-time">
              <Clock3 :size="13" aria-hidden="true" /><span>{{ t("上次运行") }}</span>
              <time v-if="task.last_run !== null && task.capabilities.last_run.available" :datetime="task.last_run">{{ task.last_run }}</time>
              <span v-else>{{ unavailableLabel(task.capabilities.last_run) }}</span>
            </div>
          </div>
          <div v-if="task.progress !== null && task.capabilities.progress.available" class="library-task-progress">
            <div class="library-progress-track" role="progressbar" :aria-valuemin="0" :aria-valuemax="100" :aria-valuenow="progressPercent(task.progress)" :aria-label="`${task.name} · ${t('进度')}: ${progressPercent(task.progress)}%`">
              <div :style="{ width: progressWidth(task.progress) }"></div>
            </div>
            <span>{{ progressPercent(task.progress) }}%</span>
          </div>
          <p v-else class="library-task-progress-unavailable">{{ t("进度") }} · {{ unavailableLabel(task.capabilities.progress) }}</p>
          <p v-if="task.error" class="library-task-error"><CircleAlert :size="14" aria-hidden="true" />{{ task.error }}</p>
        </li>
      </ul>
    </section>

    <SettingsDialogShell
      :open="dialogOpen"
      :title="t('日志')"
      :description="t('任务日志详情')"
      :close-label="t('关闭')"
      @update:open="onDetailOpenChange"
    >
      <template #icon><FileText :size="19" aria-hidden="true" /></template>
      <div
        class="library-log-detail"
        :aria-busy="failureLoading"
      >
        <p v-if="selectedTask">
          <strong>{{ t("任务名称") }}:</strong> {{ selectedTask.name }}
        </p>
        <p v-if="selectedTask">
          <strong>{{ t("服务器") }}:</strong> {{ serverName(selectedTask.server_id) }}
        </p>
        <p v-if="failureLoading" role="status" aria-live="polite">
          {{ t("加载中...") }}
        </p>
        <p
          v-else-if="failureError"
          role="alert"
          aria-live="assertive"
          class="library-error-text"
        >
          {{ failureError }}
        </p>
        <template v-else>
          <section class="mt-4">
            <h3 class="font-medium">{{ t("失败摘要") }}</h3>
            <p class="library-log-summary">
              {{ failureDetail?.summary || t("暂无详情") }}
            </p>
          </section>
          <section class="mt-5" aria-labelledby="task-failure-history-title">
            <h3 id="task-failure-history-title" class="font-medium">
              {{ t("失败历史") }}
            </h3>
            <p
              v-if="!failureDetail?.history?.length"
              class="mt-2 library-log-muted"
            >
              {{ t("暂无失败历史") }}
            </p>
            <ul v-else class="mt-2 space-y-3">
              <li
                v-for="entry in failureDetail.history"
                :key="entry.id"
                class="library-log-entry"
              >
                <p>
                  <strong>{{ t("状态") }}:</strong>
                  {{ failureStatusLabel(entry.status) }}
                </p>
                <dl class="library-log-times">
                  <div>
                    <dt class="library-log-label">
                      {{ t("观察时间") }}
                    </dt>
                    <dd>{{ formatLogTime(entry.observed_at) }}</dd>
                  </div>
                  <div>
                    <dt class="library-log-label">
                      {{ t("开始时间") }}
                    </dt>
                    <dd>{{ failureTime(entry.started_at) }}</dd>
                  </div>
                  <div>
                    <dt class="library-log-label">
                      {{ t("结束时间") }}
                    </dt>
                    <dd>{{ failureTime(entry.ended_at) }}</dd>
                  </div>
                </dl>
                <p
                  class="mt-2 break-words"
                  :class="entry.safe_error_summary ? 'library-error-text' : 'library-log-muted'"
                >
                  <strong>{{ t("错误摘要") }}:</strong>
                  {{ entry.safe_error_summary || t("无") }}
                </p>
              </li>
            </ul>
          </section>
        </template>
      </div>
    </SettingsDialogShell>
  </div>
</template>

<style scoped>
.library-tasks-page { display: grid; gap: 28px; min-width: 0; }
.library-page-head, .library-page-heading, .library-toolbar, .library-section-head,
.library-section-title, .library-card-head, .library-task-main,
.library-task-title, .library-task-actions, .library-task-progress { display: flex; align-items: center; }
.library-page-head, .library-section-head { justify-content: space-between; gap: 16px; }
.library-page-heading { gap: 14px; min-width: 0; }
.library-page-heading h2 { margin: 0; color: var(--text); font-size: var(--font-size-page); font-weight: 700; line-height: 1.3; letter-spacing: -0.5px; }
.library-page-heading p, .library-section-head > p { margin: 6px 0 0; color: var(--muted); font-size: 12px; line-height: 1.6; }
.library-page-icon, .metric-icon, .library-folder-icon, .library-task-icon { display: grid; place-items: center; flex-shrink: 0; color: var(--brand); background: var(--active-bg); }
.library-page-icon { width: 48px; height: 48px; border: 1px solid color-mix(in srgb, var(--brand) 18%, var(--border)); border-radius: 14px; }
.library-toolbar { gap: 10px; flex-shrink: 0; }
.library-server-select { position: relative; width: 200px; }
.library-server-select > svg { position: absolute; left: 12px; top: 50%; transform: translateY(-50%); color: var(--muted); pointer-events: none; }
.library-server-select select { padding-left: 36px; padding-right: 24px; border-radius: 9px; text-overflow: ellipsis; }
.library-notice { display: flex; align-items: flex-start; gap: 10px; padding: 14px 16px; border-radius: 10px; font-size: 13px; overflow-wrap: anywhere; }
.library-notice > svg { flex-shrink: 0; margin-top: 1px; }
.library-metrics { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); margin: 0; padding: 20px 8px; border: 1px solid var(--border); border-radius: 14px; background: var(--panel-bg); }
.library-metric { display: flex; align-items: center; gap: 13px; padding: 0 22px; }
.library-metric + .library-metric { border-left: 1px solid var(--border); }
.metric-icon { width: 40px; height: 40px; border-radius: 11px; }
.library-metric dl { margin: 0; min-width: 0; }
.library-metric dt { color: var(--muted); font-size: var(--font-size-label); font-weight: 700; }
.library-metric dd { margin: 3px 0 0; color: var(--text); font-size: 25px; font-weight: 700; line-height: 1.2; font-variant-numeric: tabular-nums; }
.library-section { min-width: 0; }
.library-section-head { margin-bottom: 14px; }
.library-section-head > p { margin: 0; }
.library-section-title { min-width: 0; gap: 9px; }
.library-section-title > svg { color: var(--muted); flex-shrink: 0; }
.library-section-title h3 { margin: 0; font-size: 14px; font-weight: 700; }
.library-count { display: inline-grid; place-items: center; min-width: 24px; padding: 2px 7px; border: 1px solid var(--border); border-radius: 6px; background: var(--panel-bg); color: var(--muted); font-size: 11px; font-variant-numeric: tabular-nums; }
.library-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 320px), 1fr)); align-items: start; gap: 12px; }
.library-card, .library-skeleton { min-width: 0; padding: 16px; border: 1px solid var(--border); border-radius: 10px; background: var(--panel-bg); }
.library-card { transition: border-color 0.18s ease, box-shadow 0.18s ease; }
.library-card:hover { border-color: color-mix(in srgb, var(--brand) 35%, var(--border)); box-shadow: 0 3px 12px var(--shadow-soft); }
.library-card-head { align-items: center; gap: 10px; margin-bottom: 12px; }
.library-folder-icon { width: 34px; height: 34px; border-radius: 8px; background: var(--active-bg); }
.library-card-title { min-width: 0; }
.library-card-title h4, .library-task-title h4 { margin: 0; font-size: var(--font-size-section); font-weight: 700; overflow-wrap: anywhere; }
.library-card-title p, .library-task-server { display: flex; align-items: center; gap: 5px; margin: 5px 0 0; color: var(--muted); font-size: 11px; overflow-wrap: anywhere; }
.library-card-title svg, .library-task-server svg { flex-shrink: 0; }
.library-path-list, .library-task-list { list-style: none; margin: 0; padding: 0; }
.library-path-list { border-top: 1px solid var(--border); }
.library-path-item { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 6px 10px; min-width: 0; padding-top: 12px; }
.library-path-item + .library-path-item { margin-top: 10px; border-top: 1px dashed var(--border); }
.library-path-content { min-width: 0; }
.library-path-line { display: flex; align-items: flex-start; gap: 6px; min-width: 0; color: var(--muted); }
.library-path-line > svg { flex-shrink: 0; margin-top: 3px; }
.library-path-line code { min-width: 0; color: var(--text-soft); font-size: 11px; line-height: 1.6; overflow-wrap: anywhere; }
.library-path-content > .library-status { margin-top: 5px; padding: 0; background: transparent; font-size: var(--font-size-meta); }
.library-status { display: inline-flex; align-items: center; gap: 5px; max-width: 100%; padding: 4px 7px; border-radius: 5px; font-size: var(--font-size-meta); font-weight: 600; line-height: 1.4; }
.library-status-dot { width: 5px; height: 5px; border-radius: 50%; background: currentColor; flex-shrink: 0; }
.is-neutral { background: var(--button-bg); color: var(--muted); }
.is-running { background: var(--active-bg); color: var(--brand); }
.is-success { background: var(--success-bg); color: var(--success-text); }
.is-warning { background: var(--warn-bg); color: var(--warn-text); }
.is-danger { background: var(--danger-bg); color: var(--danger-text); }
.library-validate { display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; width: 32px; height: 32px; padding: 0; border: 1px solid var(--border); border-radius: 8px; background: var(--subtle-bg); color: var(--muted); }
.library-validate:hover:not(:disabled) { border-color: color-mix(in srgb, var(--brand) 40%, var(--border)); background: var(--active-bg); color: var(--brand); }
.library-error-text { color: var(--danger-text); font-size: 12px; overflow-wrap: anywhere; }
.library-path-item > p { grid-column: 1 / -1; margin: 0; }
.library-unavailable { margin: 0; padding-top: 12px; border-top: 1px solid var(--border); color: var(--muted); font-size: var(--font-size-help); }
.library-empty { display: grid; justify-items: center; align-content: center; gap: 12px; min-height: 180px; padding: 28px 18px; border: 1px dashed var(--border-strong); border-radius: 12px; background: var(--panel-bg); color: var(--muted); text-align: center; }
.library-empty > svg { color: var(--muted); opacity: 0.65; }
.library-empty strong { font-size: var(--font-size-body); font-weight: 600; }
.library-empty p { margin: 0; font-size: 12px; line-height: 1.6; }
.library-skeleton { display: grid; align-content: center; gap: 14px; min-height: 136px; }
.skeleton-line { height: 14px; border-radius: 4px; background: var(--track-bg); }
.skeleton-line.short { width: 55%; height: 20px; }
.library-task-list { display: grid; gap: 10px; }
.library-task { min-width: 0; padding: 20px; border: 1px solid var(--border); border-radius: 12px; background: var(--panel-bg); }
.library-task.task-running { border-color: color-mix(in srgb, var(--brand) 35%, var(--border)); }
.library-task-main { gap: 12px; }
.library-task-icon { width: 38px; height: 38px; border-radius: 10px; }
.library-task-identity { min-width: 0; flex: 1; }
.library-task-title { gap: 10px; flex-wrap: wrap; }
.library-task-actions { gap: 8px; flex-shrink: 0; }
.library-task-actions button { height: 32px; min-height: 32px; padding: 0 10px; font-size: var(--font-size-control); font-weight: 600; }
.library-task-actions .task-start { border-color: color-mix(in srgb, var(--brand) 22%, var(--border)); background: var(--active-bg); color: var(--brand); }
.library-task-actions .task-start:hover:not(:disabled) { border-color: var(--brand); }
.library-task-details { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 12px 24px; margin-top: 17px; padding-top: 14px; border-top: 1px solid var(--border); }
.library-task-path, .library-task-time { display: flex; align-items: baseline; gap: 7px; min-width: 0; color: var(--muted); font-size: 11px; }
.library-task-details svg { flex-shrink: 0; align-self: flex-start; margin-top: 2px; }
.library-task-path > span, .library-task-time > span:first-of-type { flex-shrink: 0; }
.library-task-path code { color: var(--text-soft); font-size: 11px; overflow-wrap: anywhere; }
.library-task-time time { color: var(--text-soft); overflow-wrap: anywhere; }
.library-task-progress { gap: 12px; margin-top: 14px; }
.library-progress-track { height: 5px; flex: 1; overflow: hidden; border-radius: 99px; background: var(--track-bg); }
.library-progress-track > div { height: 100%; border-radius: inherit; background: var(--brand); transition: width 0.25s ease; }
.library-task-progress > span { min-width: 40px; color: var(--muted); font-size: 11px; text-align: right; font-variant-numeric: tabular-nums; }
.library-task-progress-unavailable { margin: 12px 0 0; color: var(--muted); font-size: var(--font-size-help); }
.library-task-error { display: flex; align-items: flex-start; gap: 7px; margin: 12px 0 0; padding: 10px 12px; border-radius: 6px; background: var(--danger-bg); color: var(--danger-text); font-size: 12px; overflow-wrap: anywhere; }
.library-task-error svg { flex-shrink: 0; margin-top: 2px; }
.library-log-detail { display: grid; gap: 6px; padding: 20px; overflow: auto; color: var(--text); font-size: 13px; }
.library-log-detail h3 { font-size: var(--font-size-section); font-weight: 700; }
.library-log-detail p { overflow-wrap: anywhere; }
.library-log-summary, .library-log-muted { color: var(--muted); }
.library-log-summary { margin-top: 8px; }
.library-log-entry { padding: 14px; border: 1px solid var(--border); border-radius: 8px; background: var(--subtle-bg); }
.library-log-times { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; margin-top: 12px; color: var(--text-soft); font-size: var(--font-size-meta); }
.library-log-label { margin-bottom: 3px; color: var(--muted); font-size: var(--font-size-label); font-weight: 700; }
@media (max-width: 1200px) {
  .library-metric { padding: 0 15px; }
  .library-page-head { align-items: flex-start; flex-wrap: wrap; }
}
@media (max-width: 640px) {
  .library-tasks-page { gap: 22px; }
  .library-page-icon { width: 42px; height: 42px; border-radius: 11px; }
  .library-toolbar { width: 100%; }
  .library-server-select { flex: 1; min-width: 0; }
  .library-metrics { grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 18px 0; padding: 18px 0; }
  .library-metric { gap: 10px; padding: 0 16px; }
  .library-metric:nth-child(3) { border-left: 0; }
  .library-metric dd { font-size: 22px; }
  .metric-icon { width: 34px; height: 34px; border-radius: 9px; }
  .library-grid { grid-template-columns: minmax(0, 1fr); }
  .library-task { padding: 16px; }
  .library-section-head { align-items: flex-start; flex-wrap: wrap; gap: 8px; }
  .library-task-main { flex-wrap: wrap; }
  .library-task-icon { display: none; }
  .library-task-identity { flex-basis: 100%; }
  .library-task-actions { width: 100%; justify-content: flex-end; }
  .library-task-details { grid-template-columns: minmax(0, 1fr); }
  .library-log-times { grid-template-columns: minmax(0, 1fr); }
}
@media (prefers-reduced-motion: reduce) {
  .library-tasks-page *, .library-log-detail * { animation: none !important; transition: none !important; }
}
</style>
