import { useEffect, useState } from "react";
import { Button, Form, Input, Modal, Typography, message } from "antd";
import { PictureOutlined } from "@ant-design/icons";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  closePreviewSession,
  createPreviewSession,
  getAppSettings,
  getFfmpegStatus,
} from "../services/api";

interface VideoPreviewModalProps {
  open: boolean;
  onClose: () => void;
  onOpenFfmpegSettings: () => void;
}

export function VideoPreviewModal({
  open,
  onClose,
  onOpenFfmpegSettings,
}: VideoPreviewModalProps) {
  const [form] = Form.useForm();
  const [previewing, setPreviewing] = useState(false);

  useEffect(() => {
    if (open) {
      form.resetFields();
    }
  }, [form, open]);

  const ensureFfmpegReady = async () => {
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

  const handleSubmit = async () => {
    try {
      const values = await form.validateFields();
      const rawUrl = (values.url as string | undefined)?.trim();
      if (!rawUrl) {
        return;
      }
      const extraHeaders =
        (values.extra_headers as string | undefined)?.trim() || undefined;
      const isInlineDashJson = rawUrl.startsWith("{");
      const url = isInlineDashJson ? "inline-dash-json" : rawUrl;
      const sourceKind = isInlineDashJson ? "inline_dash_json" : undefined;
      const sourceText = isInlineDashJson ? rawUrl : undefined;

      setPreviewing(true);
      if (!(await ensureFfmpegReady())) {
        return;
      }

      const { token, window_label: label } = await createPreviewSession(
        url,
        extraHeaders,
        sourceKind,
        sourceText
      );
      const previewUrl = `/?${new URLSearchParams({
        view: "preview",
        token,
      }).toString()}`;

      const previewWindow = new WebviewWindow(label, {
        url: previewUrl,
        title: "视频预览",
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
        console.error("Failed to create preview window", event);
        void closePreviewSession(token);
        message.error("打开预览窗口失败");
      });

      onClose();
    } catch (e: unknown) {
      if (e && typeof e === "object" && "errorFields" in e) return;
      message.error(`生成预览失败: ${formatError(e)}`);
    } finally {
      setPreviewing(false);
    }
  };

  return (
    <Modal
      title="视频预览图"
      open={open}
      onCancel={() => {
        if (previewing) return;
        onClose();
      }}
      maskClosable={!previewing}
      footer={[
        <Button key="cancel" onClick={onClose} disabled={previewing}>
          取消
        </Button>,
        <Button
          key="submit"
          type="primary"
          icon={<PictureOutlined />}
          loading={previewing}
          onClick={handleSubmit}
        >
          打开预览
        </Button>,
      ]}
      width={640}
      destroyOnHidden
    >
      <Form form={form} layout="vertical" preserve={false}>
        <Form.Item
          label="视频地址"
          name="url"
          rules={[{ required: true, message: "请输入视频地址" }]}
          extra="支持 m3u8、mpd 直链或 m3u8quicker-dash-v1 JSON"
        >
          <Input.TextArea
            placeholder={
              "https://example.com/video/playlist.m3u8\nhttps://example.com/video/manifest.mpd\n或粘贴 m3u8quicker-dash-v1 JSON"
            }
            autoSize={{ minRows: 3, maxRows: 6 }}
          />
        </Form.Item>
        <Form.Item
          label="附加 Header"
          name="extra_headers"
          extra="每行一个，例如：Referer: https://example.com"
        >
          <Input.TextArea
            placeholder={"Referer: https://example.com\nUser-Agent: Mozilla/5.0"}
            autoSize={{ minRows: 3, maxRows: 6 }}
          />
        </Form.Item>
      </Form>
    </Modal>
  );
}

function formatError(error: unknown): string {
  if (!error) return "未知错误";
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}
