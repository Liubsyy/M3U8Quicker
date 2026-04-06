import { useState, useEffect } from "react";
import { Modal, Form, Input, Button, Space, message } from "antd";
import { FolderOpenOutlined } from "@ant-design/icons";
import { open } from "@tauri-apps/plugin-dialog";
import { getDefaultDownloadDir, setDefaultDownloadDir } from "../services/api";
import { deriveFilenameFromUrl, type CreateDownloadParams } from "../types";

interface NewDownloadModalProps {
  open: boolean;
  initialUrl?: string;
  initialExtraHeaders?: string;
  resetKey?: number;
  onClose: () => void;
  onSubmit: (params: CreateDownloadParams) => Promise<void>;
}

export function NewDownloadModal({
  open: isOpen,
  initialUrl,
  initialExtraHeaders,
  resetKey,
  onClose,
  onSubmit,
}: NewDownloadModalProps) {
  const [form] = Form.useForm();
  const [submitting, setSubmitting] = useState(false);
  const [outputDir, setOutputDir] = useState("");
  const [filenameTouched, setFilenameTouched] = useState(false);

  useEffect(() => {
    if (isOpen) {
      getDefaultDownloadDir().then(setOutputDir);
      setFilenameTouched(false);
      form.resetFields();
      form.setFieldsValue({
        url: initialUrl || undefined,
        filename: initialUrl ? deriveFilenameFromUrl(initialUrl) || undefined : undefined,
        extra_headers: initialExtraHeaders || undefined,
      });
    }
  }, [form, initialExtraHeaders, initialUrl, isOpen, resetKey]);

  const handleSelectDir = async () => {
    const selected = await open({
      multiple: false,
      directory: true,
    });
    if (selected) {
      const selectedPath = selected as string;
      setOutputDir(selectedPath);
      await setDefaultDownloadDir(selectedPath);
    }
  };

  const handleUrlChange = (value: string) => {
    if (filenameTouched) return;

    const derived = deriveFilenameFromUrl(value);
    form.setFieldValue("filename", derived || undefined);
  };

  const handleSubmit = async () => {
    try {
      const values = await form.validateFields();
      setSubmitting(true);
      await onSubmit({
        url: values.url.trim(),
        filename: values.filename?.trim() || undefined,
        output_dir: outputDir || undefined,
        extra_headers: values.extra_headers?.trim() || undefined,
      });
      message.success("下载已开始");
    } catch (e: unknown) {
      if (e && typeof e === "object" && "errorFields" in e) return;
      message.error(`创建下载失败: ${formatCreateDownloadError(e)}`);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Modal
      title="新建下载"
      open={isOpen}
      onCancel={onClose}
      footer={null}
      destroyOnClose
      width={520}
    >
      <Form form={form} layout="vertical" onFinish={handleSubmit}>
        <Form.Item
          name="url"
          label="M3U8 地址"
          rules={[{ required: true, message: "请输入 M3U8 地址" }]}
        >
          <Input.TextArea
            placeholder="https://example.com/video/playlist.m3u8"
            rows={3}
            autoFocus
            onChange={(event) => handleUrlChange(event.target.value)}
          />
        </Form.Item>
        <Form.Item name="filename" label="文件名 (可选)">
          <Input
            placeholder="留空则自动从链接 title 或路径推导"
            onChange={(event) => {
              const value = event.target.value;
              setFilenameTouched(Boolean(value.trim()));
            }}
          />
        </Form.Item>
        <Form.Item
          name="extra_headers"
          label="附加 Header"
          extra="按行输入，每行一个 header，例如 referer:https://www.xx.com"
        >
          <Input.TextArea
            placeholder={"referer:https://www.xx.com\norigin:https://www.xx.com"}
            rows={4}
          />
        </Form.Item>
        <Form.Item label="保存目录">
          <Space.Compact style={{ width: "100%" }}>
            <Input value={outputDir} readOnly style={{ flex: 1 }} />
            <Button icon={<FolderOpenOutlined />} onClick={handleSelectDir}>
              选择
            </Button>
          </Space.Compact>
        </Form.Item>
        <Form.Item style={{ marginBottom: 0, textAlign: "right" }}>
          <Space>
            <Button onClick={onClose}>取消</Button>
            <Button type="primary" htmlType="submit" loading={submitting}>
              开始下载
            </Button>
          </Space>
        </Form.Item>
      </Form>
    </Modal>
  );
}

function formatCreateDownloadError(error: unknown) {
  const text = String(error ?? "").trim();
  if (!text) {
    return "未知错误";
  }

  const normalized = text.replace(
    /^(Invalid input|M3U8 parse error|Network error|IO error|URL parse error|Decryption error|Conversion error):\s*/i,
    ""
  );

  if (/^relative URL without a base$/i.test(normalized)) {
    return "请输入完整的 http:// 或 https:// 链接";
  }

  return normalized;
}
