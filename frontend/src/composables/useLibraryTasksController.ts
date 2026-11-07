import { computed, ref, watch } from "vue";
import type { ActionDialogRequest } from "@/composables/useActionDialog";
import type { ServerOption } from "@/types/panel";

export interface FieldCapability {
  available: boolean;
  unavailable_reason?: string;
}

export interface LibraryLocationStatus {
  path: string;
  status: string;
  error?: string | null;
}

export interface LibrarySummary {
  server_id: string;
  name: string;
  path: string | null;
  locations?: string[];
  location_statuses?: LibraryLocationStatus[];
  capabilities: {
    path: FieldCapability;
  };
}

export interface LibraryTask {
  id: string;
  name: string;
  type: string | null;
  state: "Running" | "Idle" | "Stopping" | "Completed" | "Failed" | string;
  path: string | null;
  progress: number | null;
  last_run: string | null;
  error?: string;
  server_id: string;
  capabilities: {
    path: FieldCapability;
    progress: FieldCapability;
    last_run: FieldCapability;
  };
}

export interface ServerQueryFailure {
  server_id: string;
  error: string;
}

export interface LibraryCollection<T> {
  items: T[];
  failures: ServerQueryFailure[];
}

export interface LibraryPathValidationResponse {
  items: LibraryLocationStatus[];
}

export interface TaskFailureHistory {
  id: number;
  server_id: string;
  task_id: string;
  run_identity: string;
  observed_at: number;
  status: string;
  safe_error_summary?: string | null;
  started_at?: number | null;
  ended_at?: number | null;
}

export interface TaskFailure {
  summary?: string | null;
  history?: TaskFailureHistory[];
}

type Api = <T>(path: string, init?: RequestInit) => Promise<T>;
type EncryptPayload = (name: string, value: unknown) => Promise<unknown>;
type ConfirmAction = (
  options: Omit<ActionDialogRequest, "kind">,
) => Promise<boolean>;
type PromptAction = (
  options: Omit<ActionDialogRequest, "kind">,
) => Promise<string | null>;
type Translate = (source: string) => string;
type ShowNotice = (message: string) => void;
type ClearNotice = () => void;

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function isAbortError(error: unknown) {
  if (!error || typeof error !== "object") return false;
  return (
    "name" in error &&
    ["AbortError", "TimeoutError"].includes(String(error.name))
  );
}

function normalizeCollection<T>(
  response: LibraryCollection<T> | T[],
): LibraryCollection<T> {
  if (Array.isArray(response)) return { items: response, failures: [] };
  if (!isRecord(response)) return { items: [], failures: [] };
  const items = Array.isArray(response.items) ? (response.items as T[]) : [];
  const failures = Array.isArray(response.failures)
    ? response.failures.flatMap((entry) => {
        if (
          !isRecord(entry) ||
          typeof entry.server_id !== "string" ||
          typeof entry.error !== "string"
        ) {
          return [];
        }
        return [{ server_id: entry.server_id, error: entry.error }];
      })
    : [];
  return { items, failures };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object";
}

function normalizeLocations(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter(
        (location): location is string => typeof location === "string",
      )
    : [];
}

function normalizeLocationStatuses(value: unknown): LibraryLocationStatus[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => {
    if (
      !isRecord(entry) ||
      typeof entry.path !== "string" ||
      typeof entry.status !== "string"
    ) {
      return [];
    }
    const error = entry.error;
    if (error !== undefined && error !== null && typeof error !== "string") {
      return [];
    }
    return [{ path: entry.path, status: entry.status, error }];
  });
}

function normalizePathValidationResponse(
  value: unknown,
): LibraryPathValidationResponse {
  if (!isRecord(value)) return { items: [] };
  return { items: normalizeLocationStatuses(value.items) };
}

function normalizeLibrarySummary(library: LibrarySummary): LibrarySummary {
  return {
    ...library,
    locations: normalizeLocations(library.locations),
    location_statuses: normalizeLocationStatuses(library.location_statuses),
  };
}

function normalizeFailureHistory(value: unknown): TaskFailureHistory[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => {
    if (
      !isRecord(entry) ||
      typeof entry.id !== "number" ||
      !Number.isFinite(entry.id) ||
      typeof entry.server_id !== "string" ||
      typeof entry.task_id !== "string" ||
      typeof entry.run_identity !== "string" ||
      typeof entry.observed_at !== "number" ||
      !Number.isFinite(entry.observed_at) ||
      typeof entry.status !== "string"
    ) {
      return [];
    }
    const safeErrorSummary = entry.safe_error_summary;
    const startedAt = entry.started_at;
    const endedAt = entry.ended_at;
    if (
      (safeErrorSummary !== undefined &&
        safeErrorSummary !== null &&
        typeof safeErrorSummary !== "string") ||
      (startedAt !== undefined &&
        startedAt !== null &&
        (typeof startedAt !== "number" || !Number.isFinite(startedAt))) ||
      (endedAt !== undefined &&
        endedAt !== null &&
        (typeof endedAt !== "number" || !Number.isFinite(endedAt)))
    ) {
      return [];
    }
    return [
      {
        id: entry.id,
        server_id: entry.server_id,
        task_id: entry.task_id,
        run_identity: entry.run_identity,
        observed_at: entry.observed_at,
        status: entry.status,
        safe_error_summary: safeErrorSummary,
        started_at: startedAt,
        ended_at: endedAt,
      },
    ];
  });
}

function normalizeTaskFailure(value: unknown): TaskFailure {
  if (!isRecord(value)) return { summary: null, history: [] };
  return {
    summary:
      typeof value.summary === "string" || value.summary === null
        ? value.summary
        : null,
    history: normalizeFailureHistory(value.history),
  };
}

function formatServerFailures(label: string, failures: ServerQueryFailure[]) {
  return failures.map(
    (failure) => `${label} (${failure.server_id}): ${failure.error}`,
  );
}

export function useLibraryTasksController(options: {
  api: Api;
  servers: () => ServerOption[];
  encryptPayload: EncryptPayload;
  t: Translate;
  confirmAction: ConfirmAction;
  promptAction: PromptAction;
  showNotice: ShowNotice;
  clearNotice?: ClearNotice;
  serverId?: string;
}) {
  const tasks = ref<LibraryTask[]>([]);
  const libraries = ref<LibrarySummary[]>([]);
  const failureDetail = ref<TaskFailure | null>(null);
  const selectedServer = ref(options.serverId?.trim() || "all");
  const loading = ref(false);
  const error = ref("");
  const partialError = ref("");
  const failureLoading = ref(false);
  const failureError = ref("");
  const selectedTaskId = ref<string | null>(null);
  const actionTaskId = ref<string | null>(null);
  const seenServerIds = ref<string[]>(
    options.serverId?.trim() && options.serverId.trim() !== "all"
      ? [options.serverId.trim()]
      : [],
  );

  let disposed = false;
  let refreshSeq = 0;
  let refreshController: AbortController | null = null;
  let failureSeq = 0;
  let failureController: AbortController | null = null;
  let actionController: AbortController | null = null;

  function rememberServerIds<T extends { server_id: string }>(items: T[]) {
    for (const item of items) {
      const serverId = item.server_id.trim();
      if (serverId && !seenServerIds.value.includes(serverId)) {
        seenServerIds.value.push(serverId);
      }
    }
  }

  function serverName(serverId: string) {
    return options.servers().find((server) => server.id === serverId)?.name.trim() || serverId;
  }

  const serverOptions = computed(() => {
    const ids = new Set([
      ...options.servers().map((server) => server.id),
      ...seenServerIds.value,
    ]);
    return [
      { value: "all", label: options.t("全部服务器") },
      ...Array.from(ids, (value) => ({ value, label: serverName(value) })),
    ];
  });

  const refresh = async () => {
    if (disposed) return;
    const seq = ++refreshSeq;
    refreshController?.abort();
    const controller = new AbortController();
    refreshController = controller;
    loading.value = true;
    error.value = "";
    partialError.value = "";
    // Do not render results from the previous server while the new snapshot is pending.
    tasks.value = [];
    libraries.value = [];

    const query = `?server_id=${encodeURIComponent(selectedServer.value || "all")}`;
    const tasksUrl = `/api/library/tasks${query}`;
    const librariesUrl = `/api/library/libraries${query}`;
    const [tasksResult, librariesResult] = await Promise.allSettled([
      options.api<LibraryCollection<LibraryTask> | LibraryTask[]>(tasksUrl, {
        signal: controller.signal,
      }),
      options.api<LibraryCollection<LibrarySummary> | LibrarySummary[]>(
        librariesUrl,
        { signal: controller.signal },
      ),
    ]);
    if (disposed || seq !== refreshSeq || controller.signal.aborted) return;

    const endpointErrors: string[] = [];
    const partialFailures: string[] = [];
    if (tasksResult.status === "fulfilled") {
      const result = normalizeCollection(tasksResult.value);
      tasks.value = result.items;
      rememberServerIds(result.items);
      partialFailures.push(
        ...formatServerFailures(options.t("任务名称"), result.failures),
      );
    } else if (!isAbortError(tasksResult.reason)) {
      endpointErrors.push(
        `${options.t("任务名称")}: ${errorMessage(tasksResult.reason)}`,
      );
    }
    if (librariesResult.status === "fulfilled") {
      const result = normalizeCollection(librariesResult.value);
      const normalizedLibraries = result.items.map(normalizeLibrarySummary);
      libraries.value = normalizedLibraries;
      rememberServerIds(normalizedLibraries);
      partialFailures.push(
        ...formatServerFailures(options.t("媒体库总览"), result.failures),
      );
    } else if (!isAbortError(librariesResult.reason)) {
      endpointErrors.push(
        `${options.t("媒体库总览")}: ${errorMessage(librariesResult.reason)}`,
      );
    }
    const failures = [...endpointErrors, ...partialFailures];
    if (endpointErrors.length === 2) {
      error.value = failures.join("；");
    } else if (failures.length > 0) {
      partialError.value = failures.join("；");
    }

    loading.value = false;
    if (refreshController === controller) refreshController = null;
  };

  watch(selectedServer, () => {
    void refresh();
  });

  const startTask = async (taskId: string, serverId: string) => {
    if (disposed || actionTaskId.value) return;
    const actionKey = `${serverId}:${taskId}`;
    const controller = new AbortController();
    actionController = controller;
    actionTaskId.value = actionKey;
    try {
      const confirmed = await options.confirmAction({
        title: options.t("开始扫描"),
        description: options.t("确定开始此库扫描任务？"),
        confirmText: options.t("开始"),
        cancelText: options.t("取消"),
        tone: "warning",
      });
      if (!confirmed || disposed || controller.signal.aborted) return;

      const body = await options.encryptPayload("start_library_task", {
        server_id: serverId,
      });
      if (disposed || controller.signal.aborted) return;
      await options.api(
        `/api/library/tasks/${encodeURIComponent(taskId)}/start`,
        {
          method: "POST",
          body: JSON.stringify(body),
          signal: controller.signal,
        },
      );
      if (disposed || controller.signal.aborted) return;
      await refresh();
      if (disposed || controller.signal.aborted) return;
      options.showNotice(options.t("扫描任务已启动"));
    } catch (error: unknown) {
      if (!disposed && !controller.signal.aborted && !isAbortError(error)) {
        options.showNotice(`${options.t("启动失败")}: ${errorMessage(error)}`);
      }
    } finally {
      if (actionController === controller) actionController = null;
      if (actionTaskId.value === actionKey) actionTaskId.value = null;
    }
  };

  const cancelTask = async (taskId: string, serverId: string) => {
    if (disposed || actionTaskId.value) return;
    const actionKey = `${serverId}:${taskId}`;
    const controller = new AbortController();
    actionController = controller;
    actionTaskId.value = actionKey;
    try {
      const confirmed = await options.confirmAction({
        title: options.t("取消任务"),
        description: options.t("确定取消此扫描任务？"),
        confirmText: options.t("取消任务"),
        cancelText: options.t("关闭"),
        tone: "warning",
      });
      if (!confirmed || disposed || controller.signal.aborted) return;

      const body = await options.encryptPayload("cancel_library_task", {
        server_id: serverId,
      });
      if (disposed || controller.signal.aborted) return;
      await options.api(
        `/api/library/tasks/${encodeURIComponent(taskId)}/cancel`,
        {
          method: "POST",
          body: JSON.stringify(body),
          signal: controller.signal,
        },
      );
      if (disposed || controller.signal.aborted) return;
      await refresh();
      if (disposed || controller.signal.aborted) return;
      options.showNotice(options.t("扫描任务已取消"));
    } catch (error: unknown) {
      if (!disposed && !controller.signal.aborted && !isAbortError(error)) {
        options.showNotice(`${options.t("取消失败")}: ${errorMessage(error)}`);
      }
    } finally {
      if (actionController === controller) actionController = null;
      if (actionTaskId.value === actionKey) actionTaskId.value = null;
    }
  };

  const validatePaths = async (paths: string[], serverId: string) => {
    if (disposed || actionTaskId.value) return;
    const normalizedServerId = serverId.trim();
    const normalizedPaths = paths.filter(
      (path, index, all) => path.trim() && all.indexOf(path) === index,
    );
    if (!normalizedServerId || normalizedPaths.length === 0) return;

    const actionKey = `validate:${normalizedServerId}:${normalizedPaths.join("|")}`;
    const controller = new AbortController();
    actionController = controller;
    actionTaskId.value = actionKey;
    try {
      const confirmed = await options.confirmAction({
        title: options.t("验证路径"),
        description: options.t(
          "验证路径会向远端 Emby 发起一次路径检查，确定继续吗？",
        ),
        confirmText: options.t("验证路径"),
        cancelText: options.t("关闭"),
        tone: "warning",
      });
      if (!confirmed || disposed || controller.signal.aborted) return;

      const body = await options.encryptPayload("validate_library_paths", {
        server_id: normalizedServerId,
        paths: normalizedPaths,
      });
      if (disposed || controller.signal.aborted) return;
      const raw = await options.api<unknown>(
        "/api/library/libraries/validate",
        {
          method: "POST",
          body: JSON.stringify(body),
          signal: controller.signal,
        },
      );
      if (disposed || controller.signal.aborted) return;

      const result = normalizePathValidationResponse(raw);
      if (result.items.length === 0) {
        options.showNotice(
          `${options.t("路径验证失败")}: ${options.t("不可用")}`,
        );
        return;
      }
      libraries.value = libraries.value.map((library) => {
        if (library.server_id !== normalizedServerId) return library;
        const statuses = [...(library.location_statuses ?? [])];
        for (const status of result.items) {
          const index = statuses.findIndex(
            (entry) =>
              entry.path === status.path ||
              entry.path.trim() === status.path.trim(),
          );
          if (index >= 0) statuses[index] = status;
          else statuses.push(status);
        }
        return { ...library, location_statuses: statuses };
      });
      const failed = result.items.find((item) => item.status !== "valid");
      if (failed) {
        options.showNotice(
          `${options.t("路径验证失败")}: ${failed.error || options.t("不可用")}`,
        );
      } else {
        options.showNotice(options.t("路径验证成功"));
      }
    } catch (error: unknown) {
      if (!disposed && !controller.signal.aborted && !isAbortError(error)) {
        options.showNotice(
          `${options.t("路径验证失败")}: ${errorMessage(error)}`,
        );
      }
    } finally {
      if (actionController === controller) actionController = null;
      if (actionTaskId.value === actionKey) actionTaskId.value = null;
    }
  };

  const fetchFailureDetail = async (taskId: string, serverId: string) => {
    if (disposed) return;
    const normalizedServerId = serverId.trim();
    if (!normalizedServerId) {
      failureDetail.value = null;
      failureError.value = options.t("服务器");
      return;
    }
    const seq = ++failureSeq;
    failureController?.abort();
    const controller = new AbortController();
    failureController = controller;
    failureLoading.value = true;
    failureError.value = "";
    failureDetail.value = null;
    try {
      const url = `/api/library/tasks/${encodeURIComponent(taskId)}/failure?server_id=${encodeURIComponent(normalizedServerId)}`;
      const detail = await options.api<unknown>(url, {
        signal: controller.signal,
      });
      if (disposed || seq !== failureSeq) return;
      failureDetail.value = normalizeTaskFailure(detail);
    } catch (error: unknown) {
      if (
        disposed ||
        seq !== failureSeq ||
        controller.signal.aborted ||
        isAbortError(error)
      )
        return;
      failureError.value = errorMessage(error);
    } finally {
      if (!disposed && seq === failureSeq) {
        failureLoading.value = false;
        if (failureController === controller) failureController = null;
      }
    }
  };

  const viewTaskLogs = async (task: LibraryTask) => {
    selectedTaskId.value = task.id;
    await fetchFailureDetail(task.id, task.server_id);
  };

  const closeFailureDetail = () => {
    failureSeq += 1;
    failureController?.abort();
    failureController = null;
    failureLoading.value = false;
    failureError.value = "";
    failureDetail.value = null;
    selectedTaskId.value = null;
  };

  function dispose() {
    disposed = true;
    refreshSeq += 1;
    failureSeq += 1;
    refreshController?.abort();
    failureController?.abort();
    actionController?.abort();
    refreshController = null;
    failureController = null;
    actionController = null;
  }

  return {
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
    selectedTaskId,
    actionTaskId,
    startTask,
    cancelTask,
    validatePaths,
    fetchFailureDetail,
    refresh,
    viewTaskLogs,
    closeFailureDetail,
    dispose,
  };
}

export type LibraryTasksController = ReturnType<
  typeof useLibraryTasksController
>;
