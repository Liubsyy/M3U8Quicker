import { t, useTranslation } from "../i18n";
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
  useTranslation();
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
        title: t("videoPreview2"),
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
        message.error(t("failedToOpenPreviewWindow"));
      });

      onClose();
    } catch (e: unknown) {
      if (e && typeof e === "object" && "errorFields" in e) return;
      message.error(t("failedToGeneratePreview", { value0: formatError(e) }));
    } finally {
      setPreviewing(false);
    }
  };

  return (
    <Modal
      title={t("videoThumbnails")}
      open={open}
      onCancel={() => {
        if (previewing) return;
        onClose();
      }}
      maskClosable={!previewing}
      footer={[
        <Button key="cancel" onClick={onClose} disabled={previewing}>
          {t("cancel")}</Button>,
        <Button
          key="submit"
          type="primary"
          icon={<PictureOutlined />}
          loading={previewing}
          onClick={handleSubmit}
        >
          {t("openPreview")}</Button>,
      ]}
      width={640}
      destroyOnHidden
    >
      <Form form={form} layout="vertical" preserve={false}>
        <Form.Item
          label={t("videoUrl")}
          name="url"
          rules={[{ required: true, message: t("enterAVideoUrl") }]}
          extra={t("supportsM3u8MpdDirectUrlsAndM3u8quickerDashV1Json")}
        >
          <Input.TextArea
            placeholder={
              t("httpsExampleComVideoPlaylistM3u8HttpsExampleComVideo")
            }
            autoSize={{ minRows: 3, maxRows: 6 }}
          />
        </Form.Item>
        <Form.Item
          label={t("additionalHeaders")}
          name="extra_headers"
          extra={t("onePerLineEGRefererHttpsExampleCom")}
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
  if (!error) return t("unknownError");
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}
