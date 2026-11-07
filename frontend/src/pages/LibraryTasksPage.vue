<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { Pause, Play, RefreshCw } from "@lucide/vue";
import { usePanelContext } from "@/composables/panel-context";
import {
  useLibraryTasksController,
  type LibraryLocationStatus,
  type LibrarySummary,
  type LibraryTask,
} from "@/composables/useLibraryTasksController";
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
  if (!status || status.status === "unvalidated") return "text-gray-500";
  return status.status === "valid" ? "text-green-600" : "text-red-600";
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
  <div class="p-6" :aria-busy="loading || refreshing">
    <div
      v-if="partialError"
      role="status"
      aria-live="polite"
      class="bg-yellow-100 border border-yellow-400 text-yellow-700 px-4 py-3 rounded mb-4"
    >
      {{ partialError }}
    </div>
    <div
      v-if="error"
      role="alert"
      aria-live="assertive"
      class="bg-red-100 border border-red-400 text-red-700 px-4 py-3 rounded mb-4"
    >
      {{ error }}
    </div>

    <div class="flex justify-between items-center mb-6">
      <div class="flex items-center gap-4">
        <select
          v-model="selectedServer"
          class="border p-2 rounded"
          :aria-label="t('服务器')"
        >
          <option
            v-for="option in serverOptions"
            :key="option.value"
            :value="option.value"
          >
            {{ option.label }}
          </option>
        </select>
        <button
          type="button"
          :disabled="loading || refreshing"
          class="secondary icon-button shrink-0 disabled:cursor-not-allowed disabled:opacity-50"
          :aria-label="t('刷新')"
          :title="t('刷新')"
          @click="refreshPage"
        >
          <RefreshCw class="h-4 w-4" :class="{ 'animate-spin': loading || refreshing }" aria-hidden="true" />
        </button>
      </div>
      <div class="text-sm text-gray-500">
        {{ tasks.length }} {{ t("个任务") }}
      </div>
    </div>

    <section class="mb-8">
      <h2 class="text-lg font-semibold mb-3">{{ t("媒体库总览") }}</h2>
      <div
        v-if="libraries.length === 0 && !loading"
        class="text-sm text-gray-500"
      >
        {{ t("暂无详情") }}
      </div>
      <div v-else class="grid gap-3 md:grid-cols-2 lg:grid-cols-3">
        <article
          v-for="library in libraries"
          :key="`${library.server_id}-${library.name}`"
          class="border rounded p-4"
        >
          <div class="font-medium">{{ library.name }}</div>
          <div class="text-sm text-gray-500">
            {{ t("服务器") }}: {{ serverName(library.server_id) }}
          </div>
          <div class="mt-2 text-sm text-gray-500">
            <div>{{ t("路径") }}:</div>
            <ul
              v-if="libraryLocations(library).length > 0"
              class="mt-1 space-y-2"
            >
              <li
                v-for="(path, index) in libraryLocations(library)"
                :key="`${path}-${index}`"
                class="rounded border border-gray-200 px-3 py-2"
              >
                <div class="flex items-start justify-between gap-3">
                  <code class="min-w-0 break-all text-xs text-gray-700">{{
                    path
                  }}</code>
                  <span
                    class="shrink-0 text-xs font-medium"
                    :class="locationStatusClass(locationStatus(library, path))"
                  >
                    {{ locationStatusLabel(locationStatus(library, path)) }}
                  </span>
                  <button
                    type="button"
                    class="shrink-0 text-xs text-blue-600 hover:text-blue-800 disabled:opacity-50"
                    :disabled="Boolean(actionTaskId)"
                    :aria-label="`${t('验证路径')}: ${path}`"
                    @click="validatePaths([path], library.server_id)"
                  >
                    {{ t("验证路径") }}
                  </button>
                </div>
                <p
                  v-if="locationStatus(library, path)?.error"
                  class="mt-1 break-words text-xs text-red-600"
                >
                  {{ t("错误摘要") }}:
                  {{ locationStatus(library, path)?.error }}
                </p>
              </li>
            </ul>
            <div v-else class="mt-1">
              {{ unavailableLabel(library.capabilities.path) }}
            </div>
          </div>
        </article>
      </div>
    </section>

    <div
      v-if="loading"
      role="status"
      aria-live="polite"
      class="text-center py-12"
    >
      {{ t("加载中...") }}
    </div>
    <div v-else-if="tasks.length === 0" class="text-center py-12 text-gray-500">
      {{ t("暂无库扫描任务") }}
    </div>
    <div v-else class="overflow-x-auto">
      <table class="min-w-full divide-y divide-gray-200">
        <thead>
          <tr>
            <th
              class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider"
            >
              {{ t("服务器") }}
            </th>
            <th
              class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider"
            >
              {{ t("任务名称") }}
            </th>
            <th
              class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider"
            >
              {{ t("路径") }}
            </th>
            <th
              class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider"
            >
              {{ t("状态") }}
            </th>
            <th
              class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider"
            >
              {{ t("进度") }}
            </th>
            <th
              class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider"
            >
              {{ t("时间") }}
            </th>
            <th
              class="px-6 py-3 text-left text-xs font-medium text-gray-500 uppercase tracking-wider"
            >
              {{ t("操作") }}
            </th>
          </tr>
        </thead>
        <tbody class="divide-y divide-gray-200">
          <tr v-for="task in tasks" :key="taskKey(task)">
            <td class="px-6 py-4 whitespace-nowrap">{{ serverName(task.server_id) }}</td>
            <td class="px-6 py-4">{{ task.name }}</td>
            <td class="px-6 py-4 text-sm text-gray-500">
              {{ displayCapability(task.path, task.capabilities.path) }}
            </td>
            <td class="px-6 py-4">
              <span
                :class="{
                  'text-green-600': task.state === 'Running',
                  'text-gray-500': task.state === 'Idle',
                }"
              >
                {{ taskStateLabel(task.state) }}
              </span>
              <span v-if="task.error" class="block text-xs text-red-600">{{
                task.error
              }}</span>
            </td>
            <td class="px-6 py-4 w-64">
              <template
                v-if="
                  task.progress !== null && task.capabilities.progress.available
                "
              >
                <div
                  class="bg-gray-200 h-2 rounded"
                  role="progressbar"
                  :aria-valuemin="0"
                  :aria-valuemax="100"
                  :aria-valuenow="progressPercent(task.progress)"
                  :aria-label="`${t('进度')}: ${progressPercent(task.progress)}%`"
                >
                  <div
                    class="bg-blue-600 h-2 rounded"
                    :style="{ width: progressWidth(task.progress) }"
                  ></div>
                </div>
                <div class="text-xs text-gray-500 mt-1">
                  {{ progressPercent(task.progress) }}%
                </div>
              </template>
              <span v-else class="text-sm text-gray-500">{{
                unavailableLabel(task.capabilities.progress)
              }}</span>
            </td>
            <td class="px-6 py-4 text-sm text-gray-500">
              <time
                v-if="
                  task.last_run !== null && task.capabilities.last_run.available
                "
                :datetime="task.last_run"
              >
                {{ task.last_run }}
              </time>
              <span v-else>{{
                unavailableLabel(task.capabilities.last_run)
              }}</span>
            </td>
            <td class="px-6 py-4 flex gap-3">
              <button
                v-if="task.state === 'Idle'"
                type="button"
                class="text-green-600 hover:text-green-900 disabled:opacity-50"
                :disabled="Boolean(actionTaskId)"
                :aria-label="t('开始扫描')"
                @click="startTask(task.id, task.server_id)"
              >
                <Play class="w-5 h-5" />
              </button>
              <button
                v-else-if="task.state === 'Running'"
                type="button"
                class="text-red-600 hover:text-red-900 disabled:opacity-50"
                :disabled="Boolean(actionTaskId)"
                :aria-label="t('取消任务')"
                @click="cancelTask(task.id, task.server_id)"
              >
                <Pause class="w-5 h-5" />
              </button>
              <button
                type="button"
                class="text-gray-600 hover:text-gray-900"
                :aria-label="t('日志')"
                @click="handleViewLogs(task)"
              >
                {{ t("日志") }}
              </button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <SettingsDialogShell
      :open="dialogOpen"
      :title="t('日志')"
      :description="t('任务日志详情')"
      :close-label="t('关闭')"
      @update:open="onDetailOpenChange"
    >
      <div
        class="max-h-96 overflow-auto p-4 text-sm"
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
          class="text-red-600"
        >
          {{ failureError }}
        </p>
        <template v-else>
          <section class="mt-4">
            <h3 class="font-medium">{{ t("失败摘要") }}</h3>
            <p class="mt-1 text-gray-600">
              {{ failureDetail?.summary || t("暂无详情") }}
            </p>
          </section>
          <section class="mt-5" aria-labelledby="task-failure-history-title">
            <h3 id="task-failure-history-title" class="font-medium">
              {{ t("失败历史") }}
            </h3>
            <p
              v-if="!failureDetail?.history?.length"
              class="mt-2 text-gray-500"
            >
              {{ t("暂无失败历史") }}
            </p>
            <ul v-else class="mt-2 space-y-3">
              <li
                v-for="entry in failureDetail.history"
                :key="entry.id"
                class="rounded border border-gray-200 p-3"
              >
                <p>
                  <strong>{{ t("状态") }}:</strong>
                  {{ failureStatusLabel(entry.status) }}
                </p>
                <dl class="mt-2 grid gap-2 text-gray-600 sm:grid-cols-2">
                  <div>
                    <dt class="font-medium text-gray-700">
                      {{ t("观察时间") }}
                    </dt>
                    <dd>{{ formatLogTime(entry.observed_at) }}</dd>
                  </div>
                  <div>
                    <dt class="font-medium text-gray-700">
                      {{ t("开始时间") }}
                    </dt>
                    <dd>{{ failureTime(entry.started_at) }}</dd>
                  </div>
                  <div>
                    <dt class="font-medium text-gray-700">
                      {{ t("结束时间") }}
                    </dt>
                    <dd>{{ failureTime(entry.ended_at) }}</dd>
                  </div>
                </dl>
                <p
                  class="mt-2 break-words"
                  :class="
                    entry.safe_error_summary ? 'text-red-600' : 'text-gray-500'
                  "
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
