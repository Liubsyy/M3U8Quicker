import { useEffect, useState } from "react";
import { Button, Form, Input, message, Modal, Select, Space, Typography } from "antd";
import { FolderOpenOutlined } from "@ant-design/icons";
import { open } from "@tauri-apps/plugin-dialog";
import {
  getDefaultDownloadDir,
  setDefaultDownloadDir,
} from "../services/api";
import {
  deriveFilenameFromUrl,
  type CreateLiveRecordParams,
  type LiveProtocol,
} from "../types";

interface NewLiveRecordModalProps {
  open: boolean;
  onClose: () => void;
  onSubmit: (params: CreateLiveRecordParams) => Promise<void>;
  initialUrl?: string;
  initialExtraHeaders?: string;
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
  initialUrl,
  initialExtraHeaders,
  resetKey,
}: NewLiveRecordModalProps) {
  const [form] = Form.useForm<FormValues>();
  const [submitting, setSubmitting] = useState(false);
  const [outputDir, setOutputDir] = useState("");
  const [filenameTouched, setFilenameTouched] = useState(false);

  useEffect(() => {
    if (isOpen) {
      getDefaultDownloadDir().then(setOutputDir);
      setFilenameTouched(false);
      form.resetFields();
      const initialProtocol: LiveProtocol = inferProtocolFromUrl(initialUrl ?? "");
      form.setFieldsValue({
        protocol: initialProtocol,
        url: initialUrl ?? "",
        extra_headers: initialExtraHeaders ?? "",
      });
      if (initialUrl) {
        const derived = deriveFilenameFromUrl(initialUrl);
        form.setFieldValue("filename", derived || undefined);
      }
    }
  }, [form, isOpen, initialUrl, initialExtraHeaders, resetKey]);

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

  return (
    <Modal
      title="新建直播录制"
      open={isOpen}
      onCancel={onClose}
      onOk={() => void handleSubmit()}
      okText="开始录制"
      cancelText="取消"
      confirmLoading={submitting}
      width={640}
      destroyOnClose
    >
      <Form layout="vertical" form={form} initialValues={{ protocol: "flv" }}>
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
        <Form.Item label="附加请求头" name="extra_headers">
          <Input.TextArea
            rows={3}
            placeholder={"每行一个，格式 name:value\n例如\nReferer:https://example.com\nUser-Agent:Mozilla/5.0"}
          />
        </Form.Item>
        <Typography.Paragraph type="secondary" style={{ marginBottom: 0, fontSize: 12 }}>
          直播录制会持续到主动停止；可以暂停后继续录制。
          HLS 直播会把 ts / fMP4 分片先写入临时目录，停止时再确认是否合并为 MP4。
        </Typography.Paragraph>
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
