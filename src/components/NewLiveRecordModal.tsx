import { useEffect, useState } from "react";
import { Button, Form, Input, message, Modal, Select, Space } from "antd";
import { FolderOpenOutlined } from "@ant-design/icons";
import { open } from "@tauri-apps/plugin-dialog";
import {
  getDefaultDownloadDir,
  inspectHlsTracks,
  setDefaultDownloadDir,
} from "../services/api";
import {
  deriveFilenameFromUrl,
  type FileType,
  type CreateLiveRecordParams,
  type LiveProtocol,
} from "../types";

interface NewLiveRecordModalProps {
  open: boolean;
  onClose: () => void;
  onSubmit: (params: CreateLiveRecordParams) => Promise<void>;
  onSwitchToDownload: (draft: {
    url: string;
    extraHeaders?: string;
    fileType?: FileType;
  }) => void;
  initialUrl?: string;
  initialExtraHeaders?: string;
  initialFilename?: string;
  initialOutputDir?: string;
  resetKey?: number;
}

interface FormValues {
  url: string;
  filename?: string;
  extra_headers?: string;
  protocol: LiveProtocol;
}

export function NewLiveRecordModal({
  open: isOpen,
  onClose,
  onSubmit,
  onSwitchToDownload,
  initialUrl,
  initialExtraHeaders,
  initialFilename,
  initialOutputDir,
  resetKey,
}: NewLiveRecordModalProps) {
  const [form] = Form.useForm<FormValues>();
  const [submitting, setSubmitting] = useState(false);
  const [outputDir, setOutputDir] = useState("");
  const [filenameTouched, setFilenameTouched] = useState(false);

  useEffect(() => {
    if (isOpen) {
      if (initialOutputDir) {
        setOutputDir(initialOutputDir);
      } else {
        getDefaultDownloadDir().then(setOutputDir);
      }
      setFilenameTouched(false);
      form.resetFields();
      const initialProtocol: LiveProtocol = inferProtocolFromUrl(initialUrl ?? "");
      const filename =
        initialFilename || (initialUrl ? deriveFilenameFromUrl(initialUrl) : undefined);
      form.setFieldsValue({
        protocol: initialProtocol,
        url: initialUrl ?? "",
        extra_headers: initialExtraHeaders ?? "",
        filename: filename || undefined,
      });
    }
  }, [
    form,
    initialExtraHeaders,
    initialFilename,
    initialOutputDir,
    initialUrl,
    isOpen,
    resetKey,
  ]);

  const handleSelectDir = async () => {
    const selected = await open({ multiple: false, directory: true });
    if (selected) {
      const selectedPath = selected as string;
      setOutputDir(selectedPath);
      await setDefaultDownloadDir(selectedPath);
    }
  };

  const handleUrlChange = (value: string) => {
    if (!filenameTouched) {
      const derived = deriveFilenameFromUrl(value);
      form.setFieldValue("filename", derived || undefined);
    }
    const currentProtocol = form.getFieldValue("protocol") as LiveProtocol | undefined;
    const inferred = inferProtocolFromUrl(value);
    if (inferred !== currentProtocol) {
      form.setFieldValue("protocol", inferred);
    }
  };

  const handleSubmit = async () => {
    try {
      const values = await form.validateFields();
      const url = values.url.trim();
      if (!url) {
        message.error("直播地址不能为空");
        return;
      }

      setSubmitting(true);
      const params: CreateLiveRecordParams = {
        url,
        filename: values.filename?.trim() || undefined,
        output_dir: outputDir || undefined,
        extra_headers: values.extra_headers?.trim() || undefined,
        protocol: values.protocol ?? "flv",
      };

      if (params.protocol === "hls") {
        const inspection = await inspectHlsTracks({
          url,
          extra_headers: params.extra_headers,
        });

        if (!inspection.is_live && (await confirmSwitchToDownload(params))) {
          return;
        }
      }

      await onSubmit(params);
      message.success("直播录制已开始");
      onClose();
    } catch (e: unknown) {
      if (e && typeof e === "object" && "errorFields" in e) return;
      message.error(`创建直播录制失败: ${formatError(e)}`);
    } finally {
      setSubmitting(false);
    }
  };

  const confirmSwitchToDownload = async (params: CreateLiveRecordParams) => {
    return await new Promise<boolean>((resolve) => {
      Modal.confirm({
        title: "检测到非直播 HLS",
        content: "当前地址看起来不是直播流，更适合普通下载。是否转到新建下载界面？",
        okText: "转到下载",
        cancelText: "继续录制",
        onOk: () => {
          onSwitchToDownload({
            url: params.url,
            extraHeaders: params.extra_headers,
            fileType: "hls",
          });
          resolve(true);
        },
        onCancel: () => resolve(false),
      });
    });
  };

  return (
    <Modal
      title="新建直播录制"
      open={isOpen}
      onCancel={onClose}
      footer={null}
      width={560}
      destroyOnClose
    >
      <Form
        layout="vertical"
        form={form}
        initialValues={{ protocol: "flv" }}
        onFinish={() => void handleSubmit()}
      >
        <Form.Item
          label="直播地址"
          name="url"
          rules={[{ required: true, message: "请输入直播地址" }]}
        >
          <Input
            placeholder="HTTP-FLV: https://example.com/live/stream.flv，HLS: https://example.com/live/index.m3u8"
            onChange={(e) => handleUrlChange(e.target.value)}
            allowClear
          />
        </Form.Item>
        <Form.Item label="协议" name="protocol">
          <Select
            options={[
              { value: "flv", label: "HTTP-FLV" },
              { value: "hls", label: "HLS (m3u8)" },
            ]}
          />
        </Form.Item>
        <Form.Item label="文件名（不含扩展名）" name="filename">
          <Input
            placeholder="留空则根据 URL 自动推导"
            allowClear
            onChange={() => setFilenameTouched(true)}
          />
        </Form.Item>
        <Form.Item label="保存目录">
          <Space.Compact style={{ width: "100%" }}>
            <Input value={outputDir} readOnly />
            <Button icon={<FolderOpenOutlined />} onClick={() => void handleSelectDir()}>
              选择
            </Button>
          </Space.Compact>
        </Form.Item>
        <Form.Item label="附加 Header" name="extra_headers">
          <Input.TextArea
            rows={3}
            placeholder={"每行一个，格式 name:value\n例如\nReferer:https://example.com\nUser-Agent:Mozilla/5.0"}
          />
        </Form.Item>
        <Form.Item style={{ marginBottom: 0 }}>
          <Space style={{ width: "100%", justifyContent: "flex-end" }}>
            <Button onClick={onClose}>取消</Button>
            <Button type="primary" htmlType="submit" loading={submitting}>
              开始录制
            </Button>
          </Space>
        </Form.Item>
      </Form>
    </Modal>
  );
}

function formatError(e: unknown): string {
  if (!e) return "未知错误";
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

function inferProtocolFromUrl(url: string): LiveProtocol {
  const trimmed = url.trim().toLowerCase();
  if (!trimmed) return "flv";
  const withoutQuery = trimmed.split(/[?#]/)[0] ?? trimmed;
  if (withoutQuery.endsWith(".m3u8") || withoutQuery.includes("/m3u8")) return "hls";
  if (withoutQuery.endsWith(".flv") || withoutQuery.includes("/flv")) return "flv";
  return "flv";
}
