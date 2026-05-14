import { useCallback, useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { message } from "antd";
import type {
  CreateLiveRecordParams,
  LiveGroup,
  LiveProgressEvent,
  LiveRecordCounts,
  LiveRecordPage,
  LiveRecordSummary,
} from "../types";
import * as api from "../services/api";

const DEFAULT_PAGE_SIZE = 50;

interface PageState {
  items: LiveRecordSummary[];
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

function toPageState(page: LiveRecordPage): PageState {
  return {
    items: page.items,
    total: page.total,
    page: page.page,
    pageSize: page.page_size,
  };
}

function patchPageItem(
  page: PageState,
  event: LiveProgressEvent
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
      total_bytes: event.total_bytes,
      speed_bytes_per_sec: event.speed_bytes_per_sec,
      duration_ms: event.duration_ms,
      updated_at: event.updated_at,
    };
  });
  return {
    nextPage: found ? { ...page, items } : page,
    found,
  };
}

export function useLiveRecords() {
  const [counts, setCounts] = useState<LiveRecordCounts>({
    active_count: 0,
    history_count: 0,
  });
  const [activePage, setActivePage] = useState<PageState>(INITIAL_PAGE_STATE);
  const [historyPage, setHistoryPage] = useState<PageState>(INITIAL_PAGE_STATE);
  const [loadingGroups, setLoadingGroups] = useState<Record<LiveGroup, boolean>>({
    active: true,
    history: true,
  });

  const refreshCounts = useCallback(async () => {
    const next = await api.getLiveRecordCounts();
    setCounts(next);
    return next;
  }, []);

  const refreshGroup = useCallback(
    async (group: LiveGroup, page?: number) => {
      setLoadingGroups((prev) => ({ ...prev, [group]: true }));
      try {
        const currentPage = group === "active" ? activePage : historyPage;
        const next = await api.getLiveRecordsPage(
          group,
          page ?? currentPage.page,
          currentPage.pageSize
        );
        const nextState = toPageState(next);
        if (group === "active") {
          setActivePage(nextState);
        } else {
          setHistoryPage(nextState);
        }
      } finally {
        setLoadingGroups((prev) => ({ ...prev, [group]: false }));
      }
    },
    [activePage, historyPage]
  );

  useEffect(() => {
    let disposed = false;
    const initialize = async () => {
      try {
        const [nextCounts, active, history] = await Promise.all([
          api.getLiveRecordCounts(),
          api.getLiveRecordsPage("active", 1, DEFAULT_PAGE_SIZE),
          api.getLiveRecordsPage("history", 1, DEFAULT_PAGE_SIZE),
        ]);
        if (disposed) return;
        setCounts(nextCounts);
        setActivePage(toPageState(active));
        setHistoryPage(toPageState(history));
      } catch (error) {
        console.error("Failed to initialize live records", error);
      } finally {
        if (!disposed) {
          setLoadingGroups({ active: false, history: false });
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

    listen<LiveProgressEvent>("live-progress", (event) => {
      const progress = event.payload;
      if (progress.group === "active") {
        setActivePage((prev) => patchPageItem(prev, progress).nextPage);
        return;
      }
      // Item flipped to history. Remove from active and refresh both groups.
      setActivePage((prev) => ({
        ...prev,
        items: prev.items.filter((item) => item.id !== progress.id),
        total: Math.max(0, prev.total - 1),
      }));
      void refreshCounts().catch((error) => {
        console.error("Failed to refresh live counts", error);
      });
      void refreshGroup("active").catch((error) => {
        console.error("Failed to refresh active lives", error);
      });
      void refreshGroup("history").catch((error) => {
        console.error("Failed to refresh history lives", error);
      });
    }).then((fn) => {
      unlisten = fn;
    });

    return () => {
      unlisten?.();
    };
  }, [refreshCounts, refreshGroup]);

  const addLiveRecord = useCallback(
    async (params: CreateLiveRecordParams) => {
      const task = await api.createLiveRecord(params);
      await refreshCounts();
      await refreshGroup("active", 1);
      return task;
    },
    [refreshCounts, refreshGroup]
  );

  const pause = useCallback(
    async (id: string) => {
      try {
        await api.pauseLiveRecord(id);
      } catch (error) {
        console.error("Failed to pause live record", error);
        message.error(`暂停失败: ${error}`);
      }
    },
    []
  );

  const resume = useCallback(
    async (id: string) => {
      try {
        await api.resumeLiveRecord(id);
        await refreshCounts();
        await refreshGroup("active");
      } catch (error) {
        console.error("Failed to resume live record", error);
        message.error(`恢复录制失败: ${error}`);
      }
    },
    [refreshCounts, refreshGroup]
  );

  const stop = useCallback(
    async (id: string) => {
      try {
        await api.stopLiveRecord(id);
      } catch (error) {
        console.error("Failed to stop live record", error);
        message.error(`停止失败: ${error}`);
      }
    },
    []
  );

  const cancel = useCallback(
    async (id: string) => {
      try {
        await api.cancelLiveRecord(id);
      } catch (error) {
        console.error("Failed to cancel live record", error);
        message.error(`取消失败: ${error}`);
      }
    },
    []
  );

  const remove = useCallback(
    async (id: string, deleteFile: boolean) => {
      try {
        await api.removeLiveRecord(id, deleteFile);
        await refreshCounts();
        await Promise.all([refreshGroup("active"), refreshGroup("history")]);
      } catch (error) {
        console.error("Failed to remove live record", error);
        message.error(`删除任务失败: ${error}`);
      }
    },
    [refreshCounts, refreshGroup]
  );

  const clearCompleted = useCallback(async () => {
    if (counts.history_count === 0) return;
    try {
      await api.clearLiveHistory();
      await refreshCounts();
      await refreshGroup("history", 1);
      message.success("已清空录制完成列表");
    } catch (error) {
      console.error("Failed to clear live history", error);
      message.error(`清空列表失败: ${error}`);
    }
  }, [counts.history_count, refreshCounts, refreshGroup]);

  return {
    counts,
    recording: activePage.items,
    recordingPage: activePage.page,
    recordingPageSize: activePage.pageSize,
    recordingTotal: activePage.total,
    recorded: historyPage.items,
    recordedPage: historyPage.page,
    recordedPageSize: historyPage.pageSize,
    recordedTotal: historyPage.total,
    loadingActive: loadingGroups.active,
    loadingHistory: loadingGroups.history,
    addLiveRecord,
    pause,
    resume,
    stop,
    cancel,
    remove,
    clearCompleted,
    refreshCounts,
    refreshActive: (page?: number) => refreshGroup("active", page),
    refreshHistory: (page?: number) => refreshGroup("history", page),
  };
}
