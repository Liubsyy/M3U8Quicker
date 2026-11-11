import { t, useTranslation } from "../i18n";
import { useCallback, useEffect, useRef, useState } from "react";
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

export function useLiveRecords(historyPageSize = DEFAULT_PAGE_SIZE) {
  useTranslation();
  const [counts, setCounts] = useState<LiveRecordCounts>({
    active_count: 0,
    history_count: 0,
  });
  const [activePage, setActivePage] = useState<PageState>(INITIAL_PAGE_STATE);
  const [historyPage, setHistoryPage] = useState<PageState>(INITIAL_PAGE_STATE);
  const [historySearch, setHistorySearch] = useState("");
  const [loadingGroups, setLoadingGroups] = useState<Record<LiveGroup, boolean>>({
    active: true,
    history: true,
  });
  const historyRequestIdRef = useRef(0);
  const historyQueryRef = useRef({ keyword: "", page: 1, pageSize: historyPageSize });
  const historySearchTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refreshHistory = useCallback(async (page?: number) => {
    if (historySearchTimerRef.current !== null) return;
    const query = { ...historyQueryRef.current, page: page ?? historyQueryRef.current.page };
    historyQueryRef.current.page = query.page;
    const requestId = ++historyRequestIdRef.current;
    setLoadingGroups((prev) => ({ ...prev, history: true }));
    try {
      const result = await api.getLiveRecordsPage("history", query.page, query.pageSize, query.keyword);
      if (historyRequestIdRef.current !== requestId) return;
      historyQueryRef.current.page = result.page;
      setHistoryPage(toPageState(result));
    } catch (error) {
      if (historyRequestIdRef.current !== requestId) return;
      setHistoryPage({ ...INITIAL_PAGE_STATE, pageSize: query.pageSize });
      console.error("Failed to load recording history", error);
      message.error(t("failedToLoadRecordingHistory", { value0: String(error) }));
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
    historyRequestIdRef.current += 1;
    if (historySearchTimerRef.current !== null) clearTimeout(historySearchTimerRef.current);
    setLoadingGroups((prev) => ({ ...prev, history: true }));
    historySearchTimerRef.current = setTimeout(() => {
      historySearchTimerRef.current = null;
      void refreshHistory(1);
    }, keyword ? 300 : 0);
  }, [refreshHistory]);

  const refreshCounts = useCallback(async () => {
    const next = await api.getLiveRecordCounts();
    setCounts(next);
    return next;
  }, []);

  const refreshGroup = useCallback(
    async (group: LiveGroup, page?: number) => {
      if (group === "history") return refreshHistory(page);
      setLoadingGroups((prev) => ({ ...prev, [group]: true }));
      try {
        const next = await api.getLiveRecordsPage(
          group,
          page ?? activePage.page,
          activePage.pageSize
        );
        setActivePage(toPageState(next));
      } finally {
        setLoadingGroups((prev) => ({ ...prev, [group]: false }));
      }
    },
    [activePage, refreshHistory]
  );

  useEffect(() => {
    let disposed = false;
    const initialize = async () => {
      try {
        const [nextCounts, active] = await Promise.all([
          api.getLiveRecordCounts(),
          api.getLiveRecordsPage("active", 1, DEFAULT_PAGE_SIZE),
        ]);
        if (disposed) return;
        setCounts(nextCounts);
        setActivePage(toPageState(active));
      } catch (error) {
        console.error("Failed to initialize live records", error);
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
        message.error(t("failedToPause", { value0: error }));
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
        message.error(t("failedToResumeRecording", { value0: error }));
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
        message.error(t("failedToStop", { value0: error }));
      }
    },
    []
  );

  const cancel = useCallback(
    async (id: string) => {
      try {
        await api.cancelLiveRecord(id);
        await refreshCounts();
        await Promise.all([refreshGroup("active"), refreshGroup("history")]);
      } catch (error) {
        console.error("Failed to cancel live record", error);
        message.error(t("failedToCancel", { value0: error }));
      }
    },
    [refreshCounts, refreshGroup]
  );

  const remove = useCallback(
    async (id: string, deleteFile: boolean) => {
      try {
        await api.removeLiveRecord(id, deleteFile);
        await refreshCounts();
        await Promise.all([refreshGroup("active"), refreshGroup("history")]);
      } catch (error) {
        console.error("Failed to remove live record", error);
        message.error(t("failedToDeleteTask", { value0: error }));
      }
    },
    [refreshCounts, refreshGroup]
  );

  const clearCompleted = useCallback(async () => {
    if (counts.history_count === 0 || historyQueryRef.current.keyword) return;
    try {
      await api.clearLiveHistory();
      await refreshCounts();
      await refreshGroup("history", 1);
      message.success(t("completedRecordingsCleared"));
    } catch (error) {
      console.error("Failed to clear live history", error);
      message.error(t("failedToClearList", { value0: error }));
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
    historySearch,
    searchHistory,
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
    refreshHistory,
  };
}
