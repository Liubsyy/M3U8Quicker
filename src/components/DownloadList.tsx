import { currentLanguage, t, useTranslation } from "../i18n";
import {
  Button,
  Dropdown,
  Popconfirm,
  Popover,
  Progress,
  Space,
  Spin,
  Table,
  Tag,
  Tooltip,
  Typography,
  message,
} from "antd";
import type { MenuProps } from "antd";
import type { ReactNode } from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { ColumnsType } from "antd/es/table";
import {
  CaretRightOutlined,
  CheckCircleOutlined,
  CloseCircleOutlined,
  DeleteOutlined,
  FileSearchOutlined,
  FolderOpenOutlined,
  GlobalOutlined,
  InfoCircleOutlined,
  PauseCircleOutlined,
  ReloadOutlined,
  VideoCameraOutlined,
} from "@ant-design/icons";
import type {
  DownloadTaskSegmentState,
  DownloadTaskSummary,
  DownloadStatus,
} from "../types";
import {
  canOpenInProgressPlayback,
  getFileTypeLabel,
  isDirectFileType,
} from "../types";
import { getTaskReferer, openFileLocation, openUrl } from "../services/api";

interface CancelLabels {
  title?: string;
  description?: string;
  okText?: string;
  cancelText?: string;
}

interface DownloadListProps {
  downloads: DownloadTaskSummary[];
  total: number;
  currentPage: number;
  pageSize: number;
  onPageChange: (page: number) => void;
  getSegmentState: (task: DownloadTaskSummary) => Promise<DownloadTaskSegmentState>;
  onPause: (id: string) => void;
  onResume: (id: string) => void;
  onRetryFailed: (id: string) => void;
  onCancel: (id: string) => void;
  onRemove: (id: string, deleteFile: boolean) => void;
  onStop?: (id: string) => void;
  onPlay?: (task: DownloadTaskSummary) => void;
  onAnalyze?: (filePath: string) => void;
  loading: boolean;
  showActions: ("pause" | "resume" | "cancel" | "stop" | "remove" | "open" | "play")[];
  showSpeed?: boolean;
  actionsHeaderExtra?: ReactNode;
  cancelLabels?: CancelLabels;
  statusTagOverride?: (status: DownloadStatus) => ReactNode | undefined;
  /** Override the "in progress" set when deciding which timestamp to show. */
  progressStatuses?: DownloadStatus[];
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + " " + sizes[i];
}

function formatSpeed(bytesPerSec: number): string {
  return formatBytes(bytesPerSec) + "/s";
}

function formatUpdatedAt(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "-";

  return new Intl.DateTimeFormat(currentLanguage(), {
    timeZone: "Asia/Shanghai",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(date);
}

function calculatePercentage(record: DownloadTaskSummary): number {
  if (record.status === "Completed") {
    return 100;
  }
  if (record.is_live) {
    return 0;
  }
  if (record.total_segments <= 0) {
    return 0;
  }
  return (record.completed_segments / record.total_segments) * 100;
}

function renderSegmentGrid(segmentState: DownloadTaskSegmentState) {
  const completedSet = new Set(segmentState.completed_segment_indices);
  const failedSet = new Set(segmentState.failed_segment_indices);
  const segmentItems = Array.from(
    { length: segmentState.total_segments },
    (_, index) => index + 1
  );

  return (
    <div
      className="segment-grid"
      style={{
        display: "grid",
        gridTemplateColumns: "repeat(5, minmax(36px, max-content))",
        gap: 6,
        maxHeight: 220,
        overflowY: "auto",
        paddingRight: 18,
      }}
    >
      {segmentItems.map((segmentNumber) => {
        const completed = completedSet.has(segmentNumber);
        const failed = failedSet.has(segmentNumber);
        const segmentClassName = completed
          ? "segment-chip segment-chip-completed"
          : failed
            ? "segment-chip segment-chip-failed"
            : "segment-chip segment-chip-pending";

        return (
          <div
            key={segmentNumber}
            className={segmentClassName}
            style={{
              minWidth: 36,
              padding: "2px 8px",
              textAlign: "center",
              fontSize: 12,
              lineHeight: "20px",
              fontVariantNumeric: "tabular-nums",
            }}
          >
            {segmentNumber}
          </div>
        );
      })}
    </div>
  );
}

function getStatusTag(status: DownloadStatus) {
  if (status === "Downloading") return <Tag color="processing">{t("downloading2")}</Tag>;
  if (status === "Paused") return <Tag color="warning">{t("paused")}</Tag>;
  if (status === "Completed") return <Tag color="success">{t("completed2")}</Tag>;
  if (status === "Merging") return <Tag color="warning">{t("merging")}</Tag>;
  if (status === "Converting") return <Tag color="warning">{t("converting")}</Tag>;
  if (status === "Pending") return <Tag color="default">{t("pending")}</Tag>;
  if (status === "Cancelled") return <Tag color="default">{t("cancelled")}</Tag>;
  if (typeof status === "object" && "Failed" in status)
    return <Tag color="error">{t("failed")}</Tag>;
  return <Tag>{String(status)}</Tag>;
}

export function DownloadList({
  downloads,
  total,
  currentPage,
  pageSize,
  onPageChange,
  getSegmentState,
  onPause,
  onResume,
  onRetryFailed,
  onCancel,
  onRemove,
  onStop,
  onPlay,
  onAnalyze,
  loading,
  showActions,
  showSpeed = true,
  actionsHeaderExtra,
  cancelLabels,
  statusTagOverride,
}: DownloadListProps) {
  useTranslation();
  const [segmentStates, setSegmentStates] = useState<
    Record<string, DownloadTaskSegmentState>
  >({});
  const [segmentLoading, setSegmentLoading] = useState<Record<string, boolean>>({});
  const [openSegmentPopoverId, setOpenSegmentPopoverId] = useState<string | null>(
    null
  );
  const openSegmentPopoverIdRef = useRef<string | null>(null);
  const pendingSegmentRecordRef = useRef<DownloadTaskSummary | null>(null);
  const segmentRefreshTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const segmentRequestVersionRef = useRef(0);
  const scheduledSegmentUpdateRef = useRef<Record<string, string>>({});
  const getSegmentStateRef = useRef(getSegmentState);
  const [ctxMenu, setCtxMenu] = useState<
    { record: DownloadTaskSummary; x: number; y: number } | null
  >(null);
  const [cancelConfirmId, setCancelConfirmId] = useState<string | null>(null);
  const [removeConfirmId, setRemoveConfirmId] = useState<string | null>(null);

  const buildMenuItems = (
    record: DownloadTaskSummary | undefined
  ): NonNullable<MenuProps["items"]> => {
    if (!record) return [];
    const opGroup: NonNullable<MenuProps["items"]> = [];
    const dangerGroup: NonNullable<MenuProps["items"]> = [];
    const fileGroup: NonNullable<MenuProps["items"]> = [];

    if (
      showActions.includes("play") &&
      onPlay &&
      (record.status === "Downloading" ||
        record.status === "Paused" ||
        record.status === "Completed")
    ) {
      opGroup.push({
        key: "play",
        icon: <VideoCameraOutlined />,
        label: t("play"),
        disabled: !canOpenInProgressPlayback(record),
      });
    }
    if (onAnalyze && record.status === "Completed") {
      opGroup.push({
        key: "analyze",
        icon: <FileSearchOutlined />,
        label: t("analyzeVideo"),
        disabled: !record.file_path?.trim(),
      });
    }
    if (showActions.includes("pause") && record.status === "Downloading") {
      opGroup.push({
        key: "pause",
        icon: <PauseCircleOutlined />,
        label: t("pause"),
      });
    }
    if (showActions.includes("resume") && record.status === "Paused") {
      opGroup.push({
        key: "resume",
        icon: <CaretRightOutlined />,
        label: record.is_live ? t("continueRecording") : t("resumeDownload"),
      });
    }
    if (
      showActions.includes("stop") &&
      onStop &&
      (record.status === "Downloading" || record.status === "Paused")
    ) {
      opGroup.push({
        key: "stop",
        icon: <CheckCircleOutlined />,
        label: t("stopRecording"),
      });
    }
    if (record.failed_segment_count > 0 && !record.is_live) {
      opGroup.push({
        key: "retry",
        icon: <ReloadOutlined />,
        label: t("retryFailedSegments"),
      });
    }

    if (
      showActions.includes("cancel") &&
      (record.status === "Downloading" || record.status === "Paused")
    ) {
      dangerGroup.push({
        key: "cancel",
        icon: <CloseCircleOutlined />,
        label: record.is_live ? t("cancelRecording2") : t("cancelDownload"),
        danger: true,
      });
    }
    if (showActions.includes("remove")) {
      dangerGroup.push({
        key: "remove",
        icon: <DeleteOutlined />,
        label: t("delete"),
        danger: true,
      });
    }

    if (
      showActions.includes("open") &&
      (record.file_path || record.output_dir)
    ) {
      fileGroup.push({
        key: "open",
        icon: <FolderOpenOutlined />,
        label: t("openFolder"),
      });
    }
    fileGroup.push({
      key: "open-source",
      icon: <GlobalOutlined />,
      label: t("openSourceWebsite"),
    });

    const items: NonNullable<MenuProps["items"]> = [];
    for (const g of [opGroup, dangerGroup, fileGroup]) {
      if (g.length === 0) continue;
      if (items.length > 0) items.push({ type: "divider" });
      items.push(...g);
    }
    return items;
  };

  const handleMenuClick: NonNullable<MenuProps["onClick"]> = (info) => {
    const target = ctxMenu;
    if (!target) return;
    const { record } = target;
    setCtxMenu(null);

    switch (info.key) {
      case "play":
        onPlay?.(record);
        return;
      case "analyze":
        if (record.status === "Completed" && record.file_path?.trim()) {
          onAnalyze?.(record.file_path);
        }
        return;
      case "pause":
        onPause(record.id);
        return;
      case "resume":
        onResume(record.id);
        return;
      case "stop":
        onStop?.(record.id);
        return;
      case "retry":
        onRetryFailed(record.id);
        return;
      case "cancel":
        setCancelConfirmId(record.id);
        return;
      case "remove":
        setRemoveConfirmId(record.id);
        return;
      case "open": {
        const path = record.file_path ?? record.output_dir;
        if (path) void openFileLocation(path);
        return;
      }
      case "open-source": {
        void (async () => {
          try {
            const referer = (await getTaskReferer(record.id))?.trim();
            if (!referer) {
              void message.warning(t("thisTaskHasNoRefererSoTheSourceWebsiteCannot"));
              return;
            }
            await openUrl(referer);
          } catch (error) {
            console.error("Failed to open referer:", error);
            void message.error(t("failedToOpenSourceWebsite", { value0: error }));
          }
        })();
        return;
      }
    }
  };

  useEffect(() => {
    getSegmentStateRef.current = getSegmentState;
  }, [getSegmentState]);

  const refreshSegmentState = useCallback(async (record: DownloadTaskSummary) => {
    const requestVersion = segmentRequestVersionRef.current + 1;
    segmentRequestVersionRef.current = requestVersion;
    setSegmentLoading((prev) => ({ ...prev, [record.id]: true }));

    try {
      const nextState = await getSegmentStateRef.current(record);
      if (
        segmentRequestVersionRef.current !== requestVersion ||
        openSegmentPopoverIdRef.current !== record.id
      ) {
        return;
      }
      setSegmentStates((prev) => ({
        ...prev,
        [record.id]: nextState,
      }));
    } catch (error) {
      console.error("Failed to load download segment state", error);
    } finally {
      if (segmentRequestVersionRef.current === requestVersion) {
        setSegmentLoading((prev) => ({ ...prev, [record.id]: false }));
      }
    }
  }, []);

  const scheduleSegmentRefresh = useCallback(
    (record: DownloadTaskSummary, delay: number) => {
      pendingSegmentRecordRef.current = record;
      if (segmentRefreshTimerRef.current !== null) {
        return;
      }

      segmentRefreshTimerRef.current = setTimeout(() => {
        segmentRefreshTimerRef.current = null;
        const pendingRecord = pendingSegmentRecordRef.current;
        pendingSegmentRecordRef.current = null;
        if (
          pendingRecord &&
          openSegmentPopoverIdRef.current === pendingRecord.id
        ) {
          void refreshSegmentState(pendingRecord);
        }
      }, delay);
    },
    [refreshSegmentState]
  );

  useEffect(() => {
    if (!openSegmentPopoverId) {
      return;
    }

    const record = downloads.find((item) => item.id === openSegmentPopoverId);
    if (!record) {
      return;
    }
    if (scheduledSegmentUpdateRef.current[record.id] === record.updated_at) {
      return;
    }

    scheduledSegmentUpdateRef.current[record.id] = record.updated_at;
    scheduleSegmentRefresh(record, 400);
  }, [downloads, openSegmentPopoverId, scheduleSegmentRefresh]);

  useEffect(
    () => () => {
      if (segmentRefreshTimerRef.current !== null) {
        clearTimeout(segmentRefreshTimerRef.current);
      }
      segmentRequestVersionRef.current += 1;
    },
    []
  );

  const handleSegmentPopoverOpen = (
    open: boolean,
    record: DownloadTaskSummary
  ) => {
    if (!open) {
      if (openSegmentPopoverIdRef.current !== record.id) {
        return;
      }
      openSegmentPopoverIdRef.current = null;
      setOpenSegmentPopoverId(null);
      pendingSegmentRecordRef.current = null;
      if (segmentRefreshTimerRef.current !== null) {
        clearTimeout(segmentRefreshTimerRef.current);
        segmentRefreshTimerRef.current = null;
      }
      segmentRequestVersionRef.current += 1;
      setSegmentLoading((prev) => ({ ...prev, [record.id]: false }));
      return;
    }

    if (openSegmentPopoverIdRef.current !== record.id) {
      pendingSegmentRecordRef.current = null;
      if (segmentRefreshTimerRef.current !== null) {
        clearTimeout(segmentRefreshTimerRef.current);
        segmentRefreshTimerRef.current = null;
      }
      segmentRequestVersionRef.current += 1;
    }
    openSegmentPopoverIdRef.current = record.id;
    setOpenSegmentPopoverId(record.id);
    scheduledSegmentUpdateRef.current[record.id] = record.updated_at;
    scheduleSegmentRefresh(record, 0);
  };

  const renderCompletedSegmentsPopover = (record: DownloadTaskSummary) => {
    const segmentState = segmentStates[record.id];
    const loadingSegments = segmentLoading[record.id];
    const completedSegmentCount =
      segmentState?.completed_segment_indices.length ?? record.completed_segments;
    const failedSegmentCount =
      segmentState?.failed_segment_indices.length ?? record.failed_segment_count;
    const totalSegments = segmentState?.total_segments ?? record.total_segments;

    const content = (
      <div
        style={{
          display: "inline-flex",
          flexDirection: "column",
          alignItems: "flex-start",
          paddingRight: 6,
          minWidth: 220,
        }}
      >
        <Space size={12} wrap style={{ display: "flex", marginBottom: 8 }}>
          <Typography.Text strong>{t("downloadedSegments")}</Typography.Text>
          <Typography.Text type="secondary">
            {completedSegmentCount}/{totalSegments}
          </Typography.Text>
        </Space>
        <Space size={12} wrap style={{ display: "flex", marginBottom: 12 }}>
          <Tag color="success" style={{ marginInlineEnd: 0 }}>
            {t("completed2")}</Tag>
          <Tag color="error" style={{ marginInlineEnd: 0 }}>
            {t("failed")}</Tag>
          <Tag style={{ marginInlineEnd: 0 }}>{t("incomplete")}</Tag>
        </Space>
        {failedSegmentCount > 0 ? (
          <Button
            type="link"
            size="small"
            icon={<ReloadOutlined />}
            style={{ paddingInline: 0, marginBottom: 8 }}
            onClick={() => onRetryFailed(record.id)}
          >
            {t("retryFailedSegments")}</Button>
        ) : null}
        {loadingSegments && !segmentState ? <Spin size="small" /> : null}
        {segmentState ? renderSegmentGrid(segmentState) : null}
      </div>
    );

    return (
      <Popover
        content={content}
        trigger="hover"
        placement="topLeft"
        onOpenChange={(open) => {
          handleSegmentPopoverOpen(open, record);
        }}
      >
        <Typography.Text
          type="secondary"
          style={{ display: "inline-flex", cursor: "pointer" }}
        >
          <InfoCircleOutlined />
        </Typography.Text>
      </Popover>
    );
  };

  const columns: ColumnsType<DownloadTaskSummary> = [
    {
      title: t("filename"),
      key: "filename",
      render: (_, record) => (
        (() => {
          const isDirectDownload = isDirectFileType(record.file_type);

          return (
            <div
              style={{
                minWidth: 0,
                width: "100%",
              }}
            >
              <div
                title={record.filename}
                style={{
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                  lineHeight: 1.5715,
                  display: "flex",
                  alignItems: "center",
                  gap: 6,
                }}
              >
                <Tag
                  color={isDirectDownload ? "blue" : "cyan"}
                  style={{ marginInlineEnd: 0, flexShrink: 0 }}
                >
                  {getFileTypeLabel(record.file_type)}
                </Tag>
                <span
                  style={{
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                >
                  {record.filename}
                </span>
              </div>
              {record.encryption_method && (
                <Typography.Text
                  type="secondary"
                  style={{
                    display: "block",
                    fontSize: 12,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                  title={t("encryption", { value0: record.encryption_method })}
                >
                  {t("encryption2")}{record.encryption_method}
                </Typography.Text>
              )}
            </div>
          );
        })()
      ),
    },
    {
      title: t("progress"),
      key: "progress",
      width: 280,
      render: (_, record) => (
        <div>
          <Progress
            percent={Math.round(calculatePercentage(record) * 10) / 10}
            size="small"
            status={
              record.status === "Downloading" ||
              record.status === "Merging" ||
              record.status === "Converting"
                ? "active"
                : record.status === "Completed"
                  ? "success"
                  : typeof record.status === "object"
                    ? "exception"
                    : "normal"
            }
          />
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            {isDirectFileType(record.file_type) || record.is_live ? (
              <span>{formatBytes(record.total_bytes)}</span>
            ) : (
              <>
                <Space size={4}>
                  {renderCompletedSegmentsPopover(record)}
                  <span>
                    {record.completed_segments}/{record.total_segments} {t("segments")}</span>
                  {record.failed_segment_count > 0 ? (
                    <span style={{ color: "#cf1322" }}>
                      {t("failedSegmentCount", { count: record.failed_segment_count })}</span>
                  ) : null}
                </Space>
                {" | "}
                {formatBytes(record.total_bytes)}
              </>
            )}
          </Typography.Text>
        </div>
      ),
    },
    ...(showSpeed
      ? [
          {
            title: t("speed"),
            key: "speed",
            width: 120,
            render: (_: unknown, record: DownloadTaskSummary) =>
              record.status === "Downloading"
                ? formatSpeed(record.speed_bytes_per_sec)
                : "-",
          },
        ]
      : []),
    {
      title: t("status"),
      key: "status",
      width: 180,
      render: (_, record) => {
        const isOngoingStatus =
          record.status === "Downloading" ||
          record.status === "Paused" ||
          record.status === "Pending" ||
          record.status === "Merging" ||
          record.status === "Converting";
        const statusTime = formatUpdatedAt(
          isOngoingStatus ? record.created_at : record.updated_at
        );

        const overridden = statusTagOverride?.(record.status);

        return (
          <div>
            {overridden ?? getStatusTag(record.status)}
            <Typography.Text
              type="secondary"
              style={{
                display: "block",
                fontSize: 12,
                marginTop: 4,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
              title={statusTime}
            >
              {statusTime}
            </Typography.Text>
          </div>
        );
      },
    },
    {
      title: (
        <div
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 4,
          }}
        >
          <span>{t("actions")}</span>
          {actionsHeaderExtra ? (
            <Tooltip title={t("clearList")}>{actionsHeaderExtra}</Tooltip>
          ) : null}
        </div>
      ),
      key: "actions",
      width: 160,
      render: (_, record) => (
        <Space>
          {(() => {
            const canPlay = canOpenInProgressPlayback(record);
            const playTooltip = canPlay
              ? t("play")
              : record.playback_available
                ? t("thisFormatCannotBePlayedWhileDownloading")
                : t("playbackIsNotSupportedForMultiTrackDownloads");

            return (
              showActions.includes("play") &&
              onPlay &&
              (record.status === "Downloading" ||
                record.status === "Paused" ||
                record.status === "Completed") && (
                <Tooltip title={playTooltip}>
                  <Button
                    type="text"
                    icon={<VideoCameraOutlined />}
                    onClick={() => onPlay(record)}
                    size="small"
                    disabled={!canPlay}
                  />
                </Tooltip>
              )
            );
          })()}
          {showActions.includes("pause") &&
            record.status === "Downloading" && (
              <Tooltip title={t("pause")}>
                <Button
                  type="text"
                  icon={<PauseCircleOutlined />}
                  onClick={() => onPause(record.id)}
                  size="small"
                />
              </Tooltip>
            )}
          {showActions.includes("resume") &&
            record.status === "Paused" && (
              <Tooltip title={t("resumeDownload")}>
                <Button
                  type="text"
                  icon={<CaretRightOutlined />}
                  onClick={() => onResume(record.id)}
                  size="small"
                />
              </Tooltip>
            )}
          {showActions.includes("stop") &&
            onStop &&
            (record.status === "Downloading" || record.status === "Paused") && (
              <Tooltip title={t("stopRecording")}>
                <Button
                  type="text"
                  icon={<CheckCircleOutlined />}
                  size="small"
                  onClick={() => onStop(record.id)}
                />
              </Tooltip>
            )}
          {showActions.includes("cancel") &&
            (record.status === "Downloading" || record.status === "Paused") && (
              <Popconfirm
                title={cancelLabels?.title ?? t("cancelDownload2")}
                description={cancelLabels?.description ?? t("downloadedTemporarySegmentsWillBeDeleted")}
                open={cancelConfirmId === record.id}
                onOpenChange={(open) =>
                  setCancelConfirmId(open ? record.id : null)
                }
                onConfirm={() => {
                  onCancel(record.id);
                  setCancelConfirmId(null);
                }}
                onCancel={() => setCancelConfirmId(null)}
                okText={cancelLabels?.okText ?? t("confirmCancellation")}
                cancelText={cancelLabels?.cancelText ?? t("resumeDownload")}
              >
                <Tooltip title={cancelLabels?.title ?? t("cancelDownload")}>
                  <Button
                    type="text"
                    icon={<CloseCircleOutlined />}
                    danger
                    size="small"
                  />
                </Tooltip>
              </Popconfirm>
            )}
          {showActions.includes("remove") && (
            <Popconfirm
              title={t("deleteThisTask")}
              description={t("alsoDeleteItsFiles")}
              open={removeConfirmId === record.id}
              onOpenChange={(open) =>
                setRemoveConfirmId(open ? record.id : null)
              }
              onConfirm={() => {
                onRemove(record.id, true);
                setRemoveConfirmId(null);
              }}
              onCancel={() => {
                onRemove(record.id, false);
                setRemoveConfirmId(null);
              }}
              okText={t("deleteFiles")}
              cancelText={t("removeRecordOnly")}
            >
              <Tooltip title={t("delete")}>
                <Button
                  type="text"
                  icon={<DeleteOutlined />}
                  danger
                  size="small"
                />
              </Tooltip>
            </Popconfirm>
          )}
          {showActions.includes("open") &&
            (record.file_path || record.output_dir) && (
              <Tooltip title={t("openFolder")}>
                <Button
                  type="text"
                  icon={<FolderOpenOutlined />}
                  size="small"
                  onClick={() =>
                    openFileLocation(record.file_path ?? record.output_dir)
                  }
                />
              </Tooltip>
            )}
        </Space>
      ),
    },
  ];

  const menuItems = buildMenuItems(ctxMenu?.record);

  return (
    <>
      <Table
        columns={columns}
        dataSource={downloads}
        rowKey="id"
        loading={loading}
        pagination={{
          current: currentPage,
          pageSize,
          total,
          onChange: onPageChange,
          showSizeChanger: false,
        }}
        size="middle"
        tableLayout="fixed"
        locale={{ emptyText: t("noDownloadTasks") }}
        onRow={(record) => ({
          onContextMenu: (event) => {
            if (buildMenuItems(record).length === 0) return;
            event.preventDefault();
            setCtxMenu({ record, x: event.clientX, y: event.clientY });
          },
        })}
      />
      <Dropdown
        key={
          ctxMenu
            ? `${ctxMenu.record.id}-${ctxMenu.x}-${ctxMenu.y}`
            : "ctx-empty"
        }
        open={Boolean(ctxMenu)}
        onOpenChange={(open) => {
          if (!open) setCtxMenu(null);
        }}
        trigger={["click"]}
        menu={{ items: menuItems, onClick: handleMenuClick }}
        destroyOnHidden
      >
        <div
          style={{
            position: "fixed",
            left: ctxMenu?.x ?? 0,
            top: ctxMenu?.y ?? 0,
            width: 1,
            height: 1,
            pointerEvents: "none",
          }}
        />
      </Dropdown>
    </>
  );
}
