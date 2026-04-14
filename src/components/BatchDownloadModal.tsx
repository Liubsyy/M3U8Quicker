import { useEffect, useState } from "react";
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
import { FolderOpenOutlined } from "@ant-design/icons";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getDefaultDownloadDir, setDefaultDownloadDir } from "../services/api";
import {
  deriveFilenameFromUrl,
  inferDirectFileTypeFromUrl,
  type CreateDownloadParams,
} from "../types";

const { TextArea } = Input;

interface BatchDownloadModalProps {
  open: boolean;
  initialRawInput?: string;
  initialExtraHeaders?: string;
  resetKey?: number;
  onClose: () => void;
  onSubmit: (params: CreateDownloadParams) => Promise<void>;
}

interface ParsedBatchItem {
  key: string;
  lineNumber: number;
  rawLine: string;
  url: string;
  filename?: string;
  filenameEdited?: boolean;
  mode: "hls" | "direct";
  fileType: CreateDownloadParams["file_type"];
  valid: boolean;
  error?: string;
}

export function BatchDownloadModal({
  open,
  initialRawInput,
  initialExtraHeaders,
  resetKey,
  onClose,
  onSubmit,
}: BatchDownloadModalProps) {
  const [rawInput, setRawInput] = useState("");
  const [extraHeaders, setExtraHeaders] = useState("");
  const [outputDir, setOutputDir] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [parsedItems, setParsedItems] = useState<ParsedBatchItem[]>([]);

  useEffect(() => {
    if (!open) {
      return;
    }

    void getDefaultDownloadDir().then(setOutputDir);
    setRawInput(initialRawInput || "");
    setExtraHeaders(initialExtraHeaders || "");
    setParsedItems(parseBatchInput(initialRawInput || ""));
  }, [initialExtraHeaders, initialRawInput, open, resetKey]);

  useEffect(() => {
    setParsedItems(parseBatchInput(rawInput));
  }, [rawInput]);

  const validItems = parsedItems.filter((item) => item.valid);
  const invalidItems = parsedItems.filter((item) => !item.valid);

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

  const handleSubmit = async () => {
    if (validItems.length === 0) {
      message.warning("请先粘贴至少一条可用的下载地址");
      return;
    }

    if (invalidItems.length > 0) {
      message.error("存在无法解析的行，请先修正后再开始下载");
      return;
    }

    setSubmitting(true);
    const failed: Array<{ item: ParsedBatchItem; error: string }> = [];

    try {
      for (const item of validItems) {
        try {
          await onSubmit({
            url: item.url,
            filename: item.filename || undefined,
            output_dir: outputDir || undefined,
            extra_headers: extraHeaders.trim() || undefined,
            download_mode: item.mode,
            file_type: item.fileType,
          });
        } catch (error) {
          failed.push({
            item,
            error: formatBatchCreateError(error),
          });
        }
      }

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
            rows={7}
            value={rawInput}
            onChange={(event) => setRawInput(event.target.value)}
            placeholder={[
              "https://example.com/a.m3u8",
              "https://example.com/b.mp4",
              "https://example.com/c.m3u8",
            ].join("\n")}
          />
        </div>

        {parsedItems.length > 0 ? (
          <Alert
            type={invalidItems.length > 0 ? "warning" : "info"}
            showIcon
            message={`共解析 ${parsedItems.length} 条，待创建 ${validItems.length} 条${
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
                scroll={{ y: 220 }}
                columns={[
                  {
                    title: "下载方式",
                    dataIndex: "mode",
                    width: 96,
                    render: (_, record) => (
                      <Select
                        size="small"
                        value={record.mode}
                        options={[
                          { value: "hls", label: "HLS" },
                          { value: "direct", label: "Direct" },
                        ]}
                        style={{ width: "100%" }}
                        onChange={(value) => {
                          const nextMode = value as "hls" | "direct";
                          updateParsedItem(record.key, {
                            mode: nextMode,
                          });
                        }}
                      />
                    ),
                  },
                  {
                    title: "地址",
                    dataIndex: "url",
                    ellipsis: true,
                    render: (value: string, record) => (
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
                    ),
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
          <Typography.Text strong>统一 Header</Typography.Text>
          <div style={{ marginTop: 8 }}>
            <TextArea
              rows={4}
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
              disabled={validItems.length === 0}
            >
              开始批量下载
            </Button>
          </Space>
        </div>
      </Space>
    </Modal>
  );
}

function parseBatchInput(rawInput: string): ParsedBatchItem[] {
  return rawInput
    .split(/\r?\n/)
    .map((line, index) => ({ line, lineNumber: index + 1 }))
    .filter(({ line }) => line.trim())
    .map(({ line, lineNumber }) => parseBatchLine(line, lineNumber));
}

function parseBatchLine(rawLine: string, lineNumber: number): ParsedBatchItem {
  const url = rawLine.trim();
  const directFileType = inferDirectFileTypeFromUrl(url);
  const filename = deriveFilenameFromUrl(url) || undefined;

  return normalizeParsedItem({
    key: `batch-${lineNumber}`,
    lineNumber,
    rawLine,
    url,
    filename,
    filenameEdited: false,
    mode: directFileType ? "direct" : "hls",
    fileType: directFileType ?? "hls",
    valid: true,
  });
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

  const nextFileType =
    item.fileType && item.fileType !== "hls"
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
