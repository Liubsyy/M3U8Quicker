import { useCallback, useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { message } from "antd";
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

interface PageState {
  items: DownloadTaskSummary[];
  total: number;
  page: number;
  pageSize: number;
}

const INITIAL_PAGE_STATE: PageState = {
  items: [],
  total: 0,
  page: 1,
  pageSize: DEFAULT_PAGE_SIZE,
};

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

export function useDownloads() {
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
  const [segmentStateCache, setSegmentStateCache] = useState<
    Record<string, DownloadTaskSegmentState>
  >({});

  const refreshCounts = useCallback(async () => {
    const nextCounts = await api.getDownloadCounts();
    setCounts(nextCounts);
    return nextCounts;
  }, []);

  const refreshGroup = useCallback(async (group: DownloadGroup, page?: number) => {
    setLoadingGroups((prev) => ({ ...prev, [group]: true }));
    try {
      const currentPage = group === "active" ? activePage : historyPage;
      const nextPage = await api.getDownloadsPage(
        group,
        page ?? currentPage.page,
        currentPage.pageSize
      );
      const nextState = toPageState(nextPage);

      if (group === "active") {
        setActivePage(nextState);
      } else {
        setHistoryPage(nextState);
      }
    } finally {
      setLoadingGroups((prev) => ({ ...prev, [group]: false }));
    }
  }, [activePage, historyPage]);

  useEffect(() => {
    let disposed = false;

    const initialize = async () => {
      try {
        const [nextCounts, active, history] = await Promise.all([
          api.getDownloadCounts(),
          api.getDownloadsPage("active", 1, DEFAULT_PAGE_SIZE),
          api.getDownloadsPage("history", 1, DEFAULT_PAGE_SIZE),
        ]);
        if (disposed) {
          return;
        }
        setCounts(nextCounts);
        setActivePage(toPageState(active));
        setHistoryPage(toPageState(history));
      } catch (error) {
        console.error("Failed to initialize downloads", error);
      } finally {
        if (!disposed) {
          setLoadingGroups({
            active: false,
            history: false,
          });
        }
      }
    };

    void initialize();
    return () => {
      disposed = true;
    };
  }, []);

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

  const pause = useCallback(async (id: string) => {
    await api.pauseDownload(id);
  }, []);

  const resume = useCallback(async (id: string) => {
    const task = await api.resumeDownload(id);
    await refreshCounts();
    await refreshGroup("active");
    return task;
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
      setSegmentStateCache((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
    } catch (error) {
      console.error("Failed to remove download:", error);
      message.error(`删除任务失败: ${error}`);
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
      message.success("已清空完成列表");
    } catch (error) {
      console.error("Failed to clear history downloads:", error);
      message.error(`清空列表失败: ${error}`);
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
    loadingActive: loadingGroups.active,
    loadingHistory: loadingGroups.history,
    addDownload,
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
