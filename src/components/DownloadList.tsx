import {
  Button,
  Popconfirm,
  Popover,
  Progress,
  Space,
  Spin,
  Table,
  Tag,
  Tooltip,
  Typography,
} from "antd";
import type { ReactNode } from "react";
import { useState } from "react";
import type { ColumnsType } from "antd/es/table";
import {
  CaretRightOutlined,
  CloseCircleOutlined,
  DeleteOutlined,
  FolderOpenOutlined,
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
import { openFileLocation } from "../services/api";

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
  onPlay?: (id: string) => void;
  loading: boolean;
  showActions: ("pause" | "resume" | "cancel" | "remove" | "open" | "play")[];
  showSpeed?: boolean;
  actionsHeaderExtra?: ReactNode;
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

  return new Intl.DateTimeFormat("zh-CN", {
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
        const border = completed
          ? "1px solid #95de64"
          : failed
            ? "1px solid #ff7875"
            : "1px solid #d9d9d9";
        const background = completed
          ? "#f6ffed"
          : failed
            ? "#fff2f0"
            : "#fafafa";
        const color = completed ? "#389e0d" : failed ? "#cf1322" : "#8c8c8c";

        return (
          <div
            key={segmentNumber}
            style={{
              minWidth: 36,
              padding: "2px 8px",
              borderRadius: 999,
              border,
              background,
              color,
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
  if (status === "Downloading") return <Tag color="processing">下载中</Tag>;
  if (status === "Paused") return <Tag color="warning">已暂停</Tag>;
  if (status === "Completed") return <Tag color="success">已完成</Tag>;
  if (status === "Merging") return <Tag color="warning">合并中</Tag>;
  if (status === "Converting") return <Tag color="warning">转换中</Tag>;
  if (status === "Pending") return <Tag color="default">等待中</Tag>;
  if (status === "Cancelled") return <Tag color="default">已取消</Tag>;
  if (typeof status === "object" && "Failed" in status)
    return <Tag color="error">失败</Tag>;
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
  onPlay,
  loading,
  showActions,
  showSpeed = true,
  actionsHeaderExtra,
}: DownloadListProps) {
  const [segmentStates, setSegmentStates] = useState<
    Record<string, DownloadTaskSegmentState>
  >({});
  const [segmentLoading, setSegmentLoading] = useState<Record<string, boolean>>({});

  const handleSegmentPopoverOpen = async (
    open: boolean,
    record: DownloadTaskSummary
  ) => {
    if (!open) {
      return;
    }

    const cached = segmentStates[record.id];
    if (cached && cached.updated_at === record.updated_at) {
      return;
    }

    setSegmentLoading((prev) => ({ ...prev, [record.id]: true }));
    try {
      const nextState = await getSegmentState(record);
      setSegmentStates((prev) => ({
        ...prev,
        [record.id]: nextState,
      }));
    } finally {
      setSegmentLoading((prev) => ({ ...prev, [record.id]: false }));
    }
  };

  const renderCompletedSegmentsPopover = (record: DownloadTaskSummary) => {
    const segmentState = segmentStates[record.id];
    const loadingSegments = segmentLoading[record.id];

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
          <Typography.Text strong>已下载切片</Typography.Text>
          <Typography.Text type="secondary">
            {record.completed_segments}/{record.total_segments}
          </Typography.Text>
        </Space>
        <Space size={12} wrap style={{ display: "flex", marginBottom: 12 }}>
          <Tag color="success" style={{ marginInlineEnd: 0 }}>
            已完成
          </Tag>
          <Tag color="error" style={{ marginInlineEnd: 0 }}>
            失败
          </Tag>
          <Tag style={{ marginInlineEnd: 0 }}>未完成</Tag>
        </Space>
        {record.failed_segment_count > 0 ? (
          <Button
            type="link"
            size="small"
            icon={<ReloadOutlined />}
            style={{ paddingInline: 0, marginBottom: 8 }}
            onClick={() => onRetryFailed(record.id)}
          >
            重试失败分片
          </Button>
        ) : null}
        {loadingSegments ? <Spin size="small" /> : null}
        {!loadingSegments && segmentState ? renderSegmentGrid(segmentState) : null}
      </div>
    );

    return (
      <Popover
        content={content}
        trigger="hover"
        placement="topLeft"
        onOpenChange={(open) => {
          void handleSegmentPopoverOpen(open, record);
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
      title: "文件名",
      key: "filename",
      render: (_, record) => (
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
            }}
          >
            {record.filename}
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
              title={`加密方式：${record.encryption_method}`}
            >
              加密方式：{record.encryption_method}
            </Typography.Text>
          )}
        </div>
      ),
    },
    {
      title: "进度",
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
            <Space size={4}>
              {renderCompletedSegmentsPopover(record)}
              <span>
                {record.completed_segments}/{record.total_segments} 片段
              </span>
              {record.failed_segment_count > 0 ? (
                <span style={{ color: "#cf1322" }}>
                  失败 {record.failed_segment_count} 片
                </span>
              ) : null}
            </Space>
            {" | "}
            {formatBytes(record.total_bytes)}
          </Typography.Text>
        </div>
      ),
    },
    ...(showSpeed
      ? [
          {
            title: "速度",
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
      title: "状态",
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

        return (
          <div>
            {getStatusTag(record.status)}
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
          <span>操作</span>
          {actionsHeaderExtra ? (
            <Tooltip title="清空列表">{actionsHeaderExtra}</Tooltip>
          ) : null}
        </div>
      ),
      key: "actions",
      width: 160,
      render: (_, record) => (
        <Space>
          {showActions.includes("play") &&
            onPlay &&
            (record.status === "Downloading" ||
              record.status === "Paused" ||
              record.status === "Completed") && (
              <Tooltip title="播放">
                <Button
                  type="text"
                  icon={<VideoCameraOutlined />}
                  onClick={() => onPlay(record.id)}
                  size="small"
                />
              </Tooltip>
            )}
          {showActions.includes("pause") &&
            record.status === "Downloading" && (
              <Tooltip title="暂停">
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
              <Tooltip title="继续下载">
                <Button
                  type="text"
                  icon={<CaretRightOutlined />}
                  onClick={() => onResume(record.id)}
                  size="small"
                />
              </Tooltip>
            )}
          {showActions.includes("cancel") &&
            (record.status === "Downloading" || record.status === "Paused") && (
              <Popconfirm
                title="确认取消下载?"
                description="已下载的临时切片会被清理。"
                onConfirm={() => onCancel(record.id)}
                okText="确认取消"
                cancelText="继续下载"
              >
                <Tooltip title="取消下载">
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
              title="确认删除?"
              description="是否同时删除文件?"
              onConfirm={() => onRemove(record.id, true)}
              onCancel={() => onRemove(record.id, false)}
              okText="删除文件"
              cancelText="仅移除记录"
            >
              <Tooltip title="删除">
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
              <Tooltip title="打开文件夹">
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

  return (
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
      locale={{ emptyText: "暂无下载任务" }}
    />
  );
}
