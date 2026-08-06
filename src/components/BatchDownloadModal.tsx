import { useEffect, useState, type Key } from "react";
import {
  Alert,
  Button,
  Empty,
  Input,
  Modal,
  Select,
  Space,
  Table,
  Typography,
  message,
} from "antd";
import {
  DeleteOutlined,
  FolderOpenOutlined,
  PictureOutlined,
} from "@ant-design/icons";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  closePreviewSession,
  createPreviewSession,
  getAppSettings,
  getDefaultDownloadDir,
  getFfmpegStatus,
  setDefaultDownloadDir,
} from "../services/api";
import {
  deriveFilenameFromUrl,
  inferDirectFileTypeFromUrl,
  type CreateDownloadParams,
  type DownloadMode,
  type DownloadSourceKind,
  type FileType,
} from "../types";

const INLINE_DASH_JSON_PLACEHOLDER_URL = "inline-dash-json";
const INLINE_DASH_JSON_DISPLAY = "B 站 DASH JSON";

const { TextArea } = Input;

interface BatchDownloadModalProps {
  open: boolean;
  initialRawInput?: string;
  initialExtraHeaders?: string;
  initialFileTypes?: Array<FileType | undefined>;
  resetKey?: number;
  onClose: () => void;
  onOpenFfmpegSettings: () => void;
  onSubmit: (
    paramsList: CreateDownloadParams[]
  ) => Promise<Array<{ error?: unknown }>>;
}

interface ParsedBatchItem {
  key: string;
  lineNumber: number;
  rawLine: string;
  url: string;
  filename?: string;
  filenameEdited?: boolean;
  mode: DownloadMode;
  fileType: CreateDownloadParams["file_type"];
  valid: boolean;
  error?: string;
  sourceKind?: DownloadSourceKind;
  sourceText?: string;
}

export function BatchDownloadModal({
  open,
  initialRawInput,
  initialExtraHeaders,
  initialFileTypes,
  resetKey,
  onClose,
  onOpenFfmpegSettings,
  onSubmit,
}: BatchDownloadModalProps) {
  const [rawInput, setRawInput] = useState("");
  const [extraHeaders, setExtraHeaders] = useState("");
  const [outputDir, setOutputDir] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [previewingKey, setPreviewingKey] = useState<string | null>(null);
  const [parsedItems, setParsedItems] = useState<ParsedBatchItem[]>([]);
  const [selectedRowKeys, setSelectedRowKeys] = useState<Key[]>([]);

  useEffect(() => {
    if (!open) {
      return;
    }

    void getDefaultDownloadDir().then(setOutputDir);
    setRawInput(initialRawInput || "");
    setExtraHeaders(initialExtraHeaders || "");
    const nextItems = parseBatchInput(
      initialRawInput || "",
      initialFileTypes,
      initialRawInput
    );
    setParsedItems(nextItems);
    setSelectedRowKeys(nextItems.map((item) => item.key));
  }, [initialExtraHeaders, initialFileTypes, initialRawInput, open, resetKey]);

  useEffect(() => {
    const nextItems = parseBatchInput(rawInput, initialFileTypes, initialRawInput);
    setParsedItems(nextItems);
    setSelectedRowKeys(nextItems.map((item) => item.key));
  }, [initialFileTypes, initialRawInput, rawInput]);

  const selectedKeySet = new Set(selectedRowKeys);
  const selectedItems = parsedItems.filter((item) => selectedKeySet.has(item.key));
  const validItems = selectedItems.filter((item) => item.valid);
  const invalidItems = selectedItems.filter((item) => !item.valid);

  const handleSelectDir = async () => {
    const selected = await openDialog({
      multiple: false,
      directory: true,
    });

    if (!selected) {
      return;
    }

    const selectedPath = selected as string;
    setOutputDir(selectedPath);
    await setDefaultDownloadDir(selectedPath);
  };

  const updateParsedItem = (
    key: string,
    patch:
      | Partial<ParsedBatchItem>
      | ((current: ParsedBatchItem) => Partial<ParsedBatchItem>)
  ) => {
    setParsedItems((prev) =>
      prev.map((item) =>
        item.key === key
          ? normalizeParsedItem({
              ...item,
              ...(typeof patch === "function" ? patch(item) : patch),
            })
          : item
      )
    );
  };

  const handleDeleteItem = (item: ParsedBatchItem) => {
    setRawInput((current) => {
      const newline = current.includes("\r\n") ? "\r\n" : "\n";
      const lines = current.split(/\r?\n/);
      lines.splice(item.lineNumber - 1, 1);
      return lines.join(newline);
    });
  };

  const ensurePreviewFfmpegReady = async () => {
    try {
      const [settings, ffmpegStatus] = await Promise.all([
        getAppSettings(),
        getFfmpegStatus(),
      ]);
      if (settings.ffmpeg_enabled && ffmpegStatus.kind === "installed") {
        return true;
      }
    } catch {
      // fall through to prompt
    }

    return await new Promise<boolean>((resolve) => {
      Modal.confirm({
        title: "预览需要 FFmpeg",
        content: (
          <Typography.Paragraph style={{ marginBottom: 0 }}>
            视频预览需要 FFmpeg 抽帧，请先在设置中开启并配置 FFmpeg。
          </Typography.Paragraph>
        ),
        okText: "前往设置",
        cancelText: "取消",
        onOk: () => {
          onOpenFfmpegSettings();
          resolve(false);
        },
        onCancel: () => resolve(false),
      });
    });
  };

  const handlePreviewItem = async (item: ParsedBatchItem) => {
    if (!item.valid) {
      return;
    }

    try {
      setPreviewingKey(item.key);
      if (!(await ensurePreviewFfmpegReady())) {
        return;
      }

      const { token, window_label: label } = await createPreviewSession(
        item.sourceKind === "inline_dash_json"
          ? INLINE_DASH_JSON_PLACEHOLDER_URL
          : item.url,
        extraHeaders.trim() || undefined,
        item.sourceKind,
        item.sourceText
      );
      const previewTitle =
        item.filename?.trim() ||
        (item.sourceKind === "inline_dash_json"
          ? INLINE_DASH_JSON_DISPLAY
          : deriveFilenameFromUrl(item.url)) ||
        "视频预览";
      const previewUrl = `/?${new URLSearchParams({
        view: "preview",
        token,
        title: previewTitle,
      }).toString()}`;

      const previewWindow = new WebviewWindow(label, {
        url: previewUrl,
        title: `视频预览 - ${previewTitle}`,
        width: 960,
        height: 720,
        minWidth: 720,
        minHeight: 480,
        resizable: true,
        center: true,
      });

      previewWindow.once("tauri://created", () => {
        void previewWindow.setFocus();
      });
      previewWindow.once("tauri://error", (event) => {
        console.error("Failed to create batch preview window", event);
        void closePreviewSession(token);
        message.error("打开预览窗口失败");
      });
    } catch (error) {
      message.error(`生成预览失败: ${formatBatchCreateError(error)}`);
    } finally {
      setPreviewingKey(null);
    }
  };

  const handleSubmit = async () => {
    if (validItems.length === 0) {
      message.warning("请至少选择一条可用的下载地址");
      return;
    }

    if (invalidItems.length > 0) {
      message.error("存在无法解析的行，请先修正后再开始下载");
      return;
    }

    setSubmitting(true);
    const failed: Array<{ item: ParsedBatchItem; error: string }> = [];

    try {
      const submitResults = await onSubmit(
        validItems.map((item) => ({
          url:
            item.sourceKind === "inline_dash_json"
              ? INLINE_DASH_JSON_PLACEHOLDER_URL
              : item.url,
          filename: item.filename || undefined,
          output_dir: outputDir || undefined,
          extra_headers: extraHeaders.trim() || undefined,
          download_mode: item.mode,
          file_type: item.fileType,
          source_kind: item.sourceKind,
          source_text: item.sourceText,
        }))
      );

      submitResults.forEach((result, index) => {
        if (!result?.error) {
          return;
        }

        const item = validItems[index];
        if (item) {
          failed.push({
            item,
            error: formatBatchCreateError(result.error),
          });
        }
      });

      if (failed.length === 0) {
        message.success(`已添加 ${validItems.length} 个下载任务`);
        onClose();
        return;
      }

      if (failed.length === validItems.length) {
        message.error(`批量下载创建失败：${failed[0]?.error ?? "未知错误"}`);
        return;
      }

      message.warning(
        `已成功添加 ${validItems.length - failed.length} 个任务，失败 ${failed.length} 个`
      );
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Modal
      title="批量下载"
      open={open}
      onCancel={onClose}
      footer={null}
      destroyOnClose
      width={700}
    >
      <Space direction="vertical" size={16} style={{ width: "100%" }}>
        <div>
          <Typography.Text strong>批量内容</Typography.Text>
          <Typography.Paragraph type="secondary" style={{ margin: "6px 0 0" }}>
            按行粘贴下载地址，每行一条。
          </Typography.Paragraph>
          <TextArea
            rows={5}
            value={rawInput}
            onChange={(event) => setRawInput(event.target.value)}
            placeholder={[
              "https://example.com/a.m3u8",
              "https://example.com/b.mp4",
              "https://example.com/c.mpd",
            ].join("\n")}
          />
        </div>

        {parsedItems.length > 0 ? (
          <Alert
            type={invalidItems.length > 0 ? "warning" : "info"}
            showIcon
            message={`共解析 ${parsedItems.length} 条，已选择 ${selectedItems.length} 条，待创建 ${validItems.length} 条${
              invalidItems.length > 0 ? `，异常 ${invalidItems.length} 条` : ""
            }`}
          />
        ) : null}

        <div>
          <Typography.Text strong>解析结果</Typography.Text>
          <div style={{ marginTop: 10 }}>
            {parsedItems.length > 0 ? (
              <Table<ParsedBatchItem>
                size="small"
                rowKey="key"
                pagination={false}
                dataSource={parsedItems}
                rowSelection={{
                  selectedRowKeys,
                  onChange: setSelectedRowKeys,
                }}
                scroll={{ y: 200 }}
                columns={[
                  {
                    title: "下载方式",
                    dataIndex: "mode",
                    width: 96,
                    render: (_, record) => {
                      if (record.sourceKind === "inline_dash_json") {
                        return (
                          <Select
                            size="small"
                            value="dash"
                            disabled
                            options={[{ value: "dash", label: "DASH" }]}
                            style={{ width: "100%" }}
                          />
                        );
                      }
                      return (
                        <Select
                          size="small"
                          value={record.mode}
                          options={[
                            { value: "hls", label: "HLS" },
                            { value: "dash", label: "DASH" },
                            { value: "direct", label: "Direct" },
                          ]}
                          style={{ width: "100%" }}
                          onChange={(value) => {
                            const nextMode = value as DownloadMode;
                            updateParsedItem(record.key, {
                              mode: nextMode,
                            });
                          }}
                        />
                      );
                    },
                  },
                  {
                    title: "地址",
                    dataIndex: "url",
                    ellipsis: true,
                    render: (value: string, record) => {
                      if (record.sourceKind === "inline_dash_json") {
                        return (
                          <Typography.Text type="secondary">
                            {INLINE_DASH_JSON_DISPLAY}
                          </Typography.Text>
                        );
                      }
                      return (
                        <Space direction="vertical" size={4} style={{ width: "100%" }}>
                          <Input
                            size="small"
                            value={value}
                            onChange={(event) => {
                              const nextUrl = event.target.value;
                              updateParsedItem(record.key, (current) => ({
                                url: nextUrl,
                                filename: current.filenameEdited
                                  ? current.filename
                                  : deriveFilenameFromUrl(nextUrl) || undefined,
                              }));
                            }}
                          />
                          {!record.valid ? (
                            <Typography.Text type="danger">{record.error}</Typography.Text>
                          ) : null}
                        </Space>
                      );
                    },
                  },
                  {
                    title: "名字",
                    dataIndex: "filename",
                    width: 168,
                    ellipsis: true,
                    render: (value: string | undefined, record) => (
                      <Input
                        size="small"
                        value={value ?? ""}
                        placeholder="自动推导"
                        onChange={(event) =>
                          updateParsedItem(record.key, {
                            filename: event.target.value || undefined,
                            filenameEdited: Boolean(event.target.value.trim()),
                          })
                        }
                      />
                    ),
                  },
                  {
                    title: "操作",
                    key: "action",
                    width: 88,
                    align: "center",
                    render: (_, record) => (
                      <Space size={0}>
                        <Button
                          type="text"
                          size="small"
                          icon={<PictureOutlined />}
                          title="预览"
                          aria-label="预览此行视频"
                          loading={previewingKey === record.key}
                          disabled={!record.valid}
                          onClick={() => void handlePreviewItem(record)}
                        />
                        <Button
                          type="text"
                          danger
                          size="small"
                          icon={<DeleteOutlined />}
                          title="删除"
                          aria-label="删除此行"
                          onClick={() => handleDeleteItem(record)}
                        />
                      </Space>
                    ),
                  },
                ]}
              />
            ) : (
              <div
                style={{
                  border: "1px dashed #d9d9d9",
                  borderRadius: 8,
                  padding: "28px 16px",
                }}
              >
                <Empty
                  image={Empty.PRESENTED_IMAGE_SIMPLE}
                  description="粘贴多行内容后，这里会显示解析结果"
                />
              </div>
            )}
          </div>
        </div>

        <div>
          <Typography.Text strong>附加 Header</Typography.Text>
          <div style={{ marginTop: 8 }}>
            <TextArea
              rows={3}
              value={extraHeaders}
              onChange={(event) => setExtraHeaders(event.target.value)}
              placeholder={
                "按行输入，每行一个 header\nreferer:https://example.com\norigin:https://example.com"
              }
            />
          </div>
        </div>

        <div>
          <Typography.Text strong>下载目录</Typography.Text>
          <div style={{ marginTop: 8 }}>
            <Space.Compact style={{ width: "100%" }}>
              <Input value={outputDir} readOnly style={{ flex: 1 }} />
              <Button icon={<FolderOpenOutlined />} onClick={handleSelectDir}>
                选择
              </Button>
            </Space.Compact>
          </div>
        </div>

        <div style={{ display: "flex", justifyContent: "flex-end" }}>
          <Space>
            <Button onClick={onClose}>取消</Button>
            <Button
              type="primary"
              onClick={() => void handleSubmit()}
              loading={submitting}
              disabled={selectedItems.length === 0}
            >
              开始批量下载
            </Button>
          </Space>
        </div>
      </Space>
    </Modal>
  );
}

function parseBatchInput(
  rawInput: string,
  initialFileTypes?: Array<FileType | undefined>,
  initialRawInput?: string
): ParsedBatchItem[] {
  const initialLines = initialRawInput?.split(/\r?\n/);
  return rawInput
    .split(/\r?\n/)
    .map((line, index) => ({ line, lineNumber: index + 1 }))
    .filter(({ line }) => line.trim())
    .map(({ line, lineNumber }) => {
      const initialLine = initialLines?.[lineNumber - 1];
      const initialFileType =
        initialLine?.trim() === line.trim()
          ? initialFileTypes?.[lineNumber - 1]
          : undefined;

      return parseBatchLine(line, lineNumber, initialFileType);
    });
}

function parseBatchLine(
  rawLine: string,
  lineNumber: number,
  initialFileType?: FileType
): ParsedBatchItem {
  const trimmed = rawLine.trim();
  if (trimmed.startsWith("{")) {
    return normalizeParsedItem({
      key: `batch-${lineNumber}`,
      lineNumber,
      rawLine,
      url: trimmed,
      filename: deriveFilenameFromInlineDashJson(trimmed),
      filenameEdited: false,
      mode: "dash",
      fileType: "dash",
      valid: true,
      sourceKind: "inline_dash_json",
      sourceText: trimmed,
    });
  }

  const url = trimmed;
  const directFileType = inferDirectFileTypeFromUrl(url);
  const mode: DownloadMode = initialFileType
    ? initialFileType === "hls" || initialFileType === "dash"
      ? initialFileType
      : "direct"
    : looksLikeDashUrl(url)
      ? "dash"
      : directFileType
        ? "direct"
        : "hls";
  const filename = deriveFilenameFromUrl(url) || undefined;

  return normalizeParsedItem({
    key: `batch-${lineNumber}`,
    lineNumber,
    rawLine,
    url,
    filename,
    filenameEdited: false,
    mode,
    fileType:
      initialFileType ??
      (mode === "dash"
        ? "dash"
        : directFileType ?? "hls"),
    valid: true,
  });
}

function looksLikeDashUrl(url: string): boolean {
  const trimmed = url.trim();
  try {
    const parsed = new URL(trimmed);
    if (parsed.pathname.toLowerCase().endsWith(".mpd")) {
      return true;
    }
  } catch {
    // fall through to raw string checks
  }

  const lower = trimmed.toLowerCase();
  return lower.endsWith(".mpd") || lower.includes(".mpd?") || lower.includes(".mpd#");
}

function deriveFilenameFromInlineDashJson(raw: string): string | undefined {
  try {
    const parsed = JSON.parse(raw) as { title?: unknown };
    if (typeof parsed.title === "string") {
      const sanitized = parsed.title
        // eslint-disable-next-line no-control-regex
        .replace(/[<>:"/\\|?* -]/g, "_")
        .trim();
      if (sanitized) {
        return sanitized.endsWith(".mp4") ? sanitized : `${sanitized}.mp4`;
      }
    }
  } catch {
    // ignore, fall through
  }
  return undefined;
}

function normalizeParsedItem(item: ParsedBatchItem): ParsedBatchItem {
  const url = item.url.trim();

  if (!url) {
    return {
      ...item,
      url,
      valid: false,
      error: "未找到下载地址",
    };
  }

  if (item.sourceKind === "inline_dash_json") {
    return {
      ...item,
      url,
      mode: "dash",
      fileType: "dash",
      valid: true,
      error: undefined,
    };
  }

  try {
    const parsed = new URL(url);
    if (!["http:", "https:"].includes(parsed.protocol)) {
      return {
        ...item,
        url,
        valid: false,
        error: "只支持 http:// 或 https:// 地址",
      };
    }
  } catch {
    return {
      ...item,
      url,
      valid: false,
      error: "地址格式不正确",
    };
  }

  if (item.mode === "hls") {
    return {
      ...item,
      url,
      fileType: "hls",
      valid: true,
      error: undefined,
    };
  }

  if (item.mode === "dash") {
    return {
      ...item,
      url,
      fileType: "dash",
      valid: true,
      error: undefined,
    };
  }

  const nextFileType =
    item.fileType && item.fileType !== "hls" && item.fileType !== "dash"
      ? item.fileType
      : inferDirectFileTypeFromUrl(url) ?? "mp4";

  return {
    ...item,
    url,
    mode: "direct",
    fileType: nextFileType,
    valid: true,
    error: undefined,
  };
}


function formatBatchCreateError(error: unknown) {
  const text = String(error ?? "").trim();
  if (!text) {
    return "未知错误";
  }

  return text.replace(
    /^(Invalid input|M3U8 parse error|Network error|IO error|URL parse error|Decryption error|Conversion error):\s*/i,
    ""
  );
}
