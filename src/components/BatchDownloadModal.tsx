import { t, useTranslation } from "../i18n";
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
const INLINE_DASH_JSON_DISPLAY = () => (t("bilibiliDashJson"));

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
  useTranslation();
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
        title: t("ffmpegRequiredForPreviews"),
        content: (
          <Typography.Paragraph style={{ marginBottom: 0 }}>
            {t("videoPreviewsUseFfmpegToExtractFramesEnableAndConfigure")}</Typography.Paragraph>
        ),
        okText: t("goToSettings"),
        cancelText: t("cancel"),
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
          ? INLINE_DASH_JSON_DISPLAY()
          : deriveFilenameFromUrl(item.url)) ||
        t("videoPreview2");
      const previewUrl = `/?${new URLSearchParams({
        view: "preview",
        token,
        title: previewTitle,
      }).toString()}`;

      const previewWindow = new WebviewWindow(label, {
        url: previewUrl,
        title: t("videoPreview", { value0: previewTitle }),
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
        message.error(t("failedToOpenPreviewWindow"));
      });
    } catch (error) {
      message.error(t("failedToGeneratePreview", { value0: formatBatchCreateError(error) }));
    } finally {
      setPreviewingKey(null);
    }
  };

  const handleSubmit = async () => {
    if (validItems.length === 0) {
      message.warning(t("selectAtLeastOneValidDownloadUrl"));
      return;
    }

    if (invalidItems.length > 0) {
      message.error(t("someLinesCouldNotBeParsedFixThemBeforeStarting"));
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
        message.success(t("addedDownloadTasks", { value0: validItems.length }));
        onClose();
        return;
      }

      if (failed.length === validItems.length) {
        message.error(t("failedToCreateBatchDownloads", { value0: failed[0]?.error ?? t("unknownError") }));
        return;
      }

      message.warning(
        t("addedTasksFailed", { value0: validItems.length - failed.length, value1: failed.length })
      );
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Modal
      title={t("batchDownload")}
      open={open}
      onCancel={onClose}
      footer={null}
      destroyOnClose
      width={700}
    >
      <Space direction="vertical" size={16} style={{ width: "100%" }}>
        <div>
          <Typography.Text strong>{t("urlsToDownload")}</Typography.Text>
          <Typography.Paragraph type="secondary" style={{ margin: "6px 0 0" }}>
            {t("pasteOneDownloadUrlPerLine")}</Typography.Paragraph>
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
            message={t("parsedSelectedReady", { value0: parsedItems.length, value1: selectedItems.length, value2: validItems.length, value3: invalidItems.length > 0 ? t("invalid", { value0: invalidItems.length }) : "" })}
          />
        ) : null}

        <div>
          <Typography.Text strong>{t("parsedResults")}</Typography.Text>
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
                    title: t("downloadType"),
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
                    title: t("url"),
                    dataIndex: "url",
                    ellipsis: true,
                    render: (value: string, record) => {
                      if (record.sourceKind === "inline_dash_json") {
                        return (
                          <Typography.Text type="secondary">
                            {INLINE_DASH_JSON_DISPLAY()}
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
                    title: t("name"),
                    dataIndex: "filename",
                    width: 168,
                    ellipsis: true,
                    render: (value: string | undefined, record) => (
                      <Input
                        size="small"
                        value={value ?? ""}
                        placeholder={t("automatic")}
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
                    title: t("actions"),
                    key: "action",
                    width: 88,
                    align: "center",
                    render: (_, record) => (
                      <Space size={0}>
                        <Button
                          type="text"
                          size="small"
                          icon={<PictureOutlined />}
                          title={t("preview")}
                          aria-label={t("previewThisVideo")}
                          loading={previewingKey === record.key}
                          disabled={!record.valid}
                          onClick={() => void handlePreviewItem(record)}
                        />
                        <Button
                          type="text"
                          danger
                          size="small"
                          icon={<DeleteOutlined />}
                          title={t("delete")}
                          aria-label={t("deleteThisRow")}
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
                  description={t("pasteMultipleLinesToSeeParsedResultsHere")}
                />
              </div>
            )}
          </div>
        </div>

        <div>
          <Typography.Text strong>{t("additionalHeaders")}</Typography.Text>
          <div style={{ marginTop: 8 }}>
            <TextArea
              rows={3}
              value={extraHeaders}
              onChange={(event) => setExtraHeaders(event.target.value)}
              placeholder={
                t("oneHeaderPerLineRefererHttpsExampleComOriginHttps")
              }
            />
          </div>
        </div>

        <div>
          <Typography.Text strong>{t("downloadFolder")}</Typography.Text>
          <div style={{ marginTop: 8 }}>
            <Space.Compact style={{ width: "100%" }}>
              <Input value={outputDir} readOnly style={{ flex: 1 }} />
              <Button icon={<FolderOpenOutlined />} onClick={handleSelectDir}>
                {t("browse")}</Button>
            </Space.Compact>
          </div>
        </div>

        <div style={{ display: "flex", justifyContent: "flex-end" }}>
          <Space>
            <Button onClick={onClose}>{t("cancel")}</Button>
            <Button
              type="primary"
              onClick={() => void handleSubmit()}
              loading={submitting}
              disabled={selectedItems.length === 0}
            >
              {t("startBatchDownload")}</Button>
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
      error: t("noDownloadUrlFound"),
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
        error: t("onlyHttpAndHttpsUrlsAreSupported"),
      };
    }
  } catch {
    return {
      ...item,
      url,
      valid: false,
      error: t("invalidUrlFormat"),
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
    return t("unknownError");
  }

  return text.replace(
    /^(Invalid input|M3U8 parse error|Network error|IO error|URL parse error|Decryption error|Conversion error):\s*/i,
    ""
  );
}
