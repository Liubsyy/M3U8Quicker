import { t, useTranslation } from "../i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { message, Modal } from "antd";
import type {
  CreateDownloadParams,
  DownloadCounts,
  DownloadGroup,
  DownloadProgressEvent,
  DownloadTaskPage,
  DownloadTaskSegmentState,
  DownloadTaskSummary,
} from "../types";
import * as api from "../services/api";

const DEFAULT_PAGE_SIZE = 50;
const BATCH_CREATE_CONCURRENCY = 3;

interface PageState {
  items: DownloadTaskSummary[];
  total: number;
  page: number;
  pageSize: number;
}

interface BatchAddDownloadResult {
  task?: DownloadTaskSummary;
  error?: unknown;
}

const INITIAL_PAGE_STATE: PageState = {
  items: [],
  total: 0,
  page: 1,
  pageSize: DEFAULT_PAGE_SIZE,
};

function formatBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(k)), sizes.length - 1);
  return `${parseFloat((bytes / Math.pow(k, i)).toFixed(1))} ${sizes[i]}`;
}

function confirmRestartMp4Download(downloadedBytes: number, parallel = false): Promise<boolean> {
  return new Promise((resolve) => {
    Modal.confirm({
      title: t(parallel ? "directRestartRequired" : "serverDoesNotSupportResumingDownloads"),
      content: t(parallel ? "directRestartExplanation" : "downloadedSoFarResumingWillRestartFromTheBeginning", { value0: formatBytes(downloadedBytes) }),
      okText: t("restartDownload"),
      cancelText: t("keepPaused"),
      onOk: () => {
        resolve(true);
      },
      onCancel: () => {
        resolve(false);
      },
    });
  });
}

function toPageState(page: DownloadTaskPage): PageState {
  return {
    items: page.items,
    total: page.total,
    page: page.page,
    pageSize: page.page_size,
  };
}

function patchPageItem(
  page: PageState,
  event: DownloadProgressEvent
): { nextPage: PageState; found: boolean } {
  let found = false;
  const items = page.items.map((item) => {
    if (item.id !== event.id) {
      return item;
    }

    found = true;
    return {
      ...item,
      direct_info: event.direct_info ?? item.direct_info,
      status: event.status,
      completed_segments: event.completed_segments,
      total_segments: event.total_segments,
      failed_segment_count: event.failed_segment_count,
      total_bytes: event.total_bytes,
      speed_bytes_per_sec: event.speed_bytes_per_sec,
      updated_at: event.updated_at,
    };
  });

  return {
    nextPage: found ? { ...page, items } : page,
    found,
  };
}

export function useDownloads(historyPageSize = DEFAULT_PAGE_SIZE) {
  useTranslation();
  const [counts, setCounts] = useState<DownloadCounts>({
    active_count: 0,
    history_count: 0,
  });
  const [activePage, setActivePage] = useState<PageState>(INITIAL_PAGE_STATE);
  const [historyPage, setHistoryPage] = useState<PageState>(INITIAL_PAGE_STATE);
  const [historySearch, setHistorySearch] = useState("");
  const [loadingGroups, setLoadingGroups] = useState<Record<DownloadGroup, boolean>>({
    active: true,
    history: true,
  });
  const [segmentStateCache, setSegmentStateCache] = useState<
    Record<string, DownloadTaskSegmentState>
  >({});
  const historyRequestIdRef = useRef(0);
  const historyQueryRef = useRef({ keyword: "", page: 1, pageSize: historyPageSize });
  const historySearchTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refreshHistory = useCallback(async (page?: number) => {
    // 输入防抖期间的任务刷新由即将执行的搜索一并覆盖。
    if (historySearchTimerRef.current !== null) return;
    const query = { ...historyQueryRef.current, page: page ?? historyQueryRef.current.page };
    historyQueryRef.current.page = query.page;
    const requestId = ++historyRequestIdRef.current;
    setLoadingGroups((prev) => ({ ...prev, history: true }));
    try {
      const result = await api.getDownloadsPage("history", query.page, query.pageSize, query.keyword);
      if (historyRequestIdRef.current !== requestId) return;
      historyQueryRef.current.page = result.page;
      setHistoryPage(toPageState(result));
    } catch (error) {
      if (historyRequestIdRef.current !== requestId) return;
      setHistoryPage({ ...INITIAL_PAGE_STATE, pageSize: query.pageSize });
      console.error("Failed to load history downloads", error);
      message.error(t("failedToLoadDownloadHistory", { value0: String(error) }));
    } finally {
      if (historyRequestIdRef.current === requestId) {
        setLoadingGroups((prev) => ({ ...prev, history: false }));
      }
    }
  }, []);

  const searchHistory = useCallback((value: string) => {
    setHistorySearch(value);
    const keyword = value.trim();
    if (keyword === historyQueryRef.current.keyword) return;
    historyQueryRef.current.keyword = keyword;
    historyQueryRef.current.page = 1;
    // 立即废弃旧请求，即使新搜索还在等待防抖也不能回写旧结果。
    historyRequestIdRef.current += 1;
    if (historySearchTimerRef.current !== null) clearTimeout(historySearchTimerRef.current);
    setLoadingGroups((prev) => ({ ...prev, history: true }));
    historySearchTimerRef.current = setTimeout(() => {
      historySearchTimerRef.current = null;
      void refreshHistory(1);
    }, keyword ? 300 : 0);
  }, [refreshHistory]);

  const refreshCounts = useCallback(async () => {
    const nextCounts = await api.getDownloadCounts();
    setCounts(nextCounts);
    return nextCounts;
  }, []);

  const refreshGroup = useCallback(async (group: DownloadGroup, page?: number) => {
    if (group === "history") return refreshHistory(page);
    setLoadingGroups((prev) => ({ ...prev, [group]: true }));
    try {
      const nextPage = await api.getDownloadsPage(
        group,
        page ?? activePage.page,
        activePage.pageSize
      );
      setActivePage(toPageState(nextPage));
    } finally {
      setLoadingGroups((prev) => ({ ...prev, [group]: false }));
    }
  }, [activePage, refreshHistory]);

  useEffect(() => {
    let disposed = false;

    const initialize = async () => {
      try {
        const [nextCounts, active] = await Promise.all([
          api.getDownloadCounts(),
          api.getDownloadsPage("active", 1, DEFAULT_PAGE_SIZE),
        ]);
        if (disposed) {
          return;
        }
        setCounts(nextCounts);
        setActivePage(toPageState(active));
      } catch (error) {
        console.error("Failed to initialize downloads", error);
      } finally {
        if (!disposed) {
          setLoadingGroups((prev) => ({ ...prev, active: false }));
        }
      }
    };

    void initialize();
    return () => {
      disposed = true;
    };
  }, []);

  useEffect(() => {
    historyQueryRef.current.pageSize = historyPageSize;
    void refreshHistory(1);
    return () => {
      historyRequestIdRef.current += 1;
      if (historySearchTimerRef.current !== null) {
        clearTimeout(historySearchTimerRef.current);
        historySearchTimerRef.current = null;
      }
    };
  }, [historyPageSize, refreshHistory]);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;

    listen<DownloadProgressEvent>("download-progress", (event) => {
      const progress = event.payload;
      setSegmentStateCache((prev) => {
        const next = { ...prev };
        delete next[progress.id];
        return next;
      });

      if (progress.group === "active") {
        setActivePage((prev) => patchPageItem(prev, progress).nextPage);
        return;
      }

      setActivePage((prev) => ({
        ...prev,
        items: prev.items.filter((item) => item.id !== progress.id),
        total: Math.max(0, prev.total - 1),
      }));

      void refreshCounts().catch((error) => {
        console.error("Failed to refresh download counts", error);
      });
      void refreshGroup("active").catch((error) => {
        console.error("Failed to refresh active downloads", error);
      });
      void refreshGroup("history").catch((error) => {
        console.error("Failed to refresh history downloads", error);
      });
    }).then((fn) => {
      unlisten = fn;
    });

    return () => {
      unlisten?.();
    };
  }, [refreshCounts, refreshGroup]);

  const addDownload = useCallback(async (params: CreateDownloadParams) => {
    const task = await api.createDownload(params);
    await refreshCounts();
    await refreshGroup("active", 1);
    return task;
  }, [refreshCounts, refreshGroup]);

  const addDownloadsBatch = useCallback(
    async (paramsList: CreateDownloadParams[]): Promise<BatchAddDownloadResult[]> => {
      const results: BatchAddDownloadResult[] = Array.from(
        { length: paramsList.length },
        () => ({})
      );
      let nextIndex = 0;
      let successCount = 0;

      const workerCount = Math.min(BATCH_CREATE_CONCURRENCY, paramsList.length);
      const workers = Array.from({ length: workerCount }, async () => {
        while (true) {
          const currentIndex = nextIndex;
          nextIndex += 1;

          if (currentIndex >= paramsList.length) {
            return;
          }

          try {
            const task = await api.createDownload(paramsList[currentIndex]);
            results[currentIndex] = { task };
            successCount += 1;
          } catch (error) {
            results[currentIndex] = { error };
          }
        }
      });

      await Promise.all(workers);

      if (successCount > 0) {
        await refreshCounts();
        await refreshGroup("active", 1);
      }

      return results;
    },
    [refreshCounts, refreshGroup]
  );

  const pause = useCallback(async (id: string) => {
    await api.pauseDownload(id);
  }, []);

  const resume = useCallback(async (id: string) => {
    try {
      const check = await api.checkResumeDownload(id);
      const restartConfirmed =
        check.action === "confirm_restart"
          ? await confirmRestartMp4Download(check.downloaded_bytes, activePage.items.find((item) => item.id === id)?.direct_info?.parallel === true)
          : false;

      if (check.action === "confirm_restart" && !restartConfirmed) {
        return undefined;
      }

      const task = await api.resumeDownload(id, restartConfirmed);
      await refreshCounts();
      await refreshGroup("active");
      return task;
    } catch (error) {
      console.error("Failed to resume download:", error);
      message.error(t("theOriginalUrlIsInvalidOrExpiredCannotResumeDownload"));
      return undefined;
    }
  }, [activePage.items, refreshCounts, refreshGroup]);

  const retryFailed = useCallback(async (id: string) => {
    const task = await api.retryFailedSegments(id);
    await refreshCounts();
    await refreshGroup("active");
    return task;
  }, [refreshCounts, refreshGroup]);

  const cancel = useCallback(async (id: string) => {
    await api.cancelDownload(id);
  }, []);

  const remove = useCallback(async (id: string, deleteFile: boolean) => {
    try {
      await api.removeDownload(id, deleteFile);
      await refreshCounts();
      await Promise.all([refreshGroup("active"), refreshGroup("history")]);
      setSegmentStateCache((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
    } catch (error) {
      console.error("Failed to remove download:", error);
      message.error(t("failedToDeleteTask", { value0: error }));
    }
  }, [refreshCounts, refreshGroup]);

  const clearCompleted = useCallback(async () => {
    if (counts.history_count === 0 || historyQueryRef.current.keyword) {
      return;
    }

    try {
      await api.clearHistoryDownloads();
      await refreshCounts();
      await refreshGroup("history", 1);
      message.success(t("completedListCleared"));
    } catch (error) {
      console.error("Failed to clear history downloads:", error);
      message.error(t("failedToClearList", { value0: error }));
    }
  }, [counts.history_count, refreshCounts, refreshGroup]);

  const getSegmentState = useCallback(async (task: DownloadTaskSummary) => {
    const cached = segmentStateCache[task.id];
    if (cached && cached.updated_at === task.updated_at) {
      return cached;
    }

    const segmentState = await api.getDownloadSegmentState(task.id);
    setSegmentStateCache((prev) => ({
      ...prev,
      [task.id]: segmentState,
    }));
    return segmentState;
  }, [segmentStateCache]);

  return {
    counts,
    downloading: activePage.items,
    downloadingPage: activePage.page,
    downloadingPageSize: activePage.pageSize,
    downloadingTotal: activePage.total,
    completed: historyPage.items,
    completedPage: historyPage.page,
    completedPageSize: historyPage.pageSize,
    completedTotal: historyPage.total,
    historySearch,
    searchHistory,
    loadingActive: loadingGroups.active,
    loadingHistory: loadingGroups.history,
    addDownload,
    addDownloadsBatch,
    pause,
    resume,
    retryFailed,
    cancel,
    remove,
    clearCompleted,
    refreshCounts,
    refreshActive: (page?: number) => refreshGroup("active", page),
    refreshHistory,
    getSegmentState,
  };
}
