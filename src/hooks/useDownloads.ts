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
  DownloadTaskSummary,
} from "../types";
import * as api from "../services/api";

const DEFAULT_PAGE_SIZE = 50;
const BATCH_CREATE_CONCURRENCY = 3;
const PROGRESS_FLUSH_INTERVAL_MS = 100;

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

function confirmRestartMp4Download(downloadedBytes: number): Promise<boolean> {
  return new Promise((resolve) => {
    Modal.confirm({
      title: t("serverDoesNotSupportResumingDownloads"),
      content: t("downloadedSoFarResumingWillRestartFromTheBeginning", { value0: formatBytes(downloadedBytes) }),
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
  const [loadingGroups, setLoadingGroups] = useState<Record<DownloadGroup, boolean>>({
    active: true,
    history: true,
  });
  const activePageRef = useRef(activePage);
  const historyPageRef = useRef(historyPage);
  const pendingProgressRef = useRef(
    new Map<string, DownloadProgressEvent>()
  );
  const progressFlushTimerRef = useRef<number | null>(null);
  const historyRequestIdRef = useRef(0);

  useEffect(() => {
    activePageRef.current = activePage;
  }, [activePage]);

  useEffect(() => {
    historyPageRef.current = historyPage;
  }, [historyPage]);

  const refreshCounts = useCallback(async () => {
    const nextCounts = await api.getDownloadCounts();
    setCounts(nextCounts);
    return nextCounts;
  }, []);

  const refreshGroup = useCallback(async (group: DownloadGroup, page?: number) => {
    setLoadingGroups((prev) => ({ ...prev, [group]: true }));
    try {
      const currentPage =
        group === "active" ? activePageRef.current : historyPageRef.current;
      const pageSize =
        group === "history" ? historyPageSize : currentPage.pageSize;
      const nextPage = await api.getDownloadsPage(
        group,
        page ?? currentPage.page,
        pageSize
      );
      const nextState = toPageState(nextPage);

      if (group === "active") {
        activePageRef.current = nextState;
        setActivePage(nextState);
      } else {
        historyPageRef.current = nextState;
        setHistoryPage(nextState);
      }
    } finally {
      setLoadingGroups((prev) => ({ ...prev, [group]: false }));
    }
  }, [historyPageSize]);

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
        const nextActivePage = toPageState(active);
        setCounts(nextCounts);
        activePageRef.current = nextActivePage;
        setActivePage(nextActivePage);
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
    let disposed = false;
    const requestId = historyRequestIdRef.current + 1;
    historyRequestIdRef.current = requestId;

    setLoadingGroups((prev) => ({ ...prev, history: true }));
    void api
      .getDownloadsPage("history", 1, historyPageSize)
      .then((history) => {
        if (!disposed && historyRequestIdRef.current === requestId) {
          const nextHistoryPage = toPageState(history);
          historyPageRef.current = nextHistoryPage;
          setHistoryPage(nextHistoryPage);
        }
      })
      .catch((error) => {
        console.error("Failed to load history downloads", error);
      })
      .finally(() => {
        if (!disposed && historyRequestIdRef.current === requestId) {
          setLoadingGroups((prev) => ({ ...prev, history: false }));
        }
      });

    return () => {
      disposed = true;
    };
  }, [historyPageSize]);

  const flushPendingProgress = useCallback(() => {
    progressFlushTimerRef.current = null;
    if (pendingProgressRef.current.size === 0) return;

    const events = Array.from(pendingProgressRef.current.values());
    pendingProgressRef.current.clear();
    const hasTerminalEvent = events.some((event) => event.group !== "active");

    setActivePage((prev) => {
      let next = prev;

      for (const event of events) {
        if (event.group === "active") {
          next = patchPageItem(next, event).nextPage;
          continue;
        }

        const itemCount = next.items.length;
        const items = next.items.filter((item) => item.id !== event.id);
        if (items.length !== itemCount) {
          next = {
            ...next,
            items,
            total: Math.max(0, next.total - 1),
          };
        }
      }

      activePageRef.current = next;
      return next;
    });

    if (!hasTerminalEvent) return;

    void refreshCounts().catch((error) => {
      console.error("Failed to refresh download counts", error);
    });
    void refreshGroup("active").catch((error) => {
      console.error("Failed to refresh active downloads", error);
    });
    void refreshGroup("history", 1).catch((error) => {
      console.error("Failed to refresh history downloads", error);
    });
  }, [refreshCounts, refreshGroup]);

  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    const pendingProgress = pendingProgressRef.current;

    void listen<DownloadProgressEvent>("download-progress", ({ payload }) => {
      pendingProgress.set(payload.id, payload);
      if (progressFlushTimerRef.current !== null) return;

      progressFlushTimerRef.current = window.setTimeout(
        flushPendingProgress,
        PROGRESS_FLUSH_INTERVAL_MS
      );
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    }).catch((error) => {
      console.error("Failed to subscribe to download progress", error);
    });

    return () => {
      disposed = true;
      unlisten?.();
      if (progressFlushTimerRef.current !== null) {
        window.clearTimeout(progressFlushTimerRef.current);
        progressFlushTimerRef.current = null;
      }
      pendingProgress.clear();
    };
  }, [flushPendingProgress]);

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
          ? await confirmRestartMp4Download(check.downloaded_bytes)
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
  }, [refreshCounts, refreshGroup]);

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
    } catch (error) {
      console.error("Failed to remove download:", error);
      message.error(t("failedToDeleteTask", { value0: error }));
    }
  }, [refreshCounts, refreshGroup]);

  const clearCompleted = useCallback(async () => {
    if (counts.history_count === 0) {
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
    return api.getDownloadSegmentState(task.id);
  }, []);

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
    refreshHistory: (page?: number) => refreshGroup("history", page),
    getSegmentState,
  };
}
