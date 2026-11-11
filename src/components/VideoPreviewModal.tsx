import { t, useTranslation } from "../i18n";
import { useEffect, useState } from "react";
import { Button, Form, Input, Modal, Segmented, Typography, message } from "antd";
import { FolderOpenOutlined, PictureOutlined } from "@ant-design/icons";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open as pickFile } from "@tauri-apps/plugin-dialog";
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
  const [source, setSource] = useState<"url" | "file">("url");

  useEffect(() => {
    if (open) {
      form.resetFields();
      setSource("url");
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
    let sessionToken: string | undefined;
    try {
      const values = await form.validateFields();
      const localPath = source === "file" ? values.local_path as string : undefined;
      const rawUrl = (values.url as string | undefined)?.trim();
      if (!localPath && !rawUrl) {
        return;
      }
      const extraHeaders =
        source === "url" ? (values.extra_headers as string | undefined)?.trim() || undefined : undefined;
      const isInlineDashJson = !localPath && rawUrl?.startsWith("{");
      const url = isInlineDashJson ? "inline-dash-json" : rawUrl ?? "";
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
        sourceText,
        localPath
      );
      sessionToken = token;
      const title = localPath?.split(/[\\/]/).pop();
      const previewUrl = `/?${new URLSearchParams({
        view: "preview",
        token,
        ...(title ? { title } : {}),
      }).toString()}`;

      const previewWindow = new WebviewWindow(label, {
        url: previewUrl,
        title: title ? t("videoPreview", { value0: title }) : t("videoPreview2"),
        width: 960,
        height: 720,
        minWidth: 720,
        minHeight: 480,
        resizable: true,
        center: true,
      });

      previewWindow.once("tauri://created", () => {
        void (async () => {
          await previewWindow.show();
          if (await previewWindow.isMinimized()) {
            await previewWindow.unminimize();
          }
          await previewWindow.setFocus();
        })().catch((error) => {
          console.error("Failed to bring preview window to front", error);
        });
      });
      previewWindow.once("tauri://error", (event) => {
        console.error("Failed to create preview window", event);
        void closePreviewSession(token);
        message.error(t("failedToOpenPreviewWindow"));
      });

      onClose();
    } catch (e: unknown) {
      if (sessionToken) void closePreviewSession(sessionToken);
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
        <Segmented
          value={source}
          disabled={previewing}
          options={[
            { label: t("networkLink"), value: "url" },
            { label: t("localFile"), value: "file" },
          ]}
          onChange={(value) => setSource(value as "url" | "file")}
          style={{ marginBottom: 16 }}
        />
        {source === "file" ? (
          <>
            <Form.Item name="local_path" label={t("localFile")}
              rules={[{ required: true, whitespace: true, message: t("selectAVideoFile") }]}>
              <Input placeholder={t("selectAVideoFile")} disabled={previewing} />
            </Form.Item>
            <Button icon={<FolderOpenOutlined />} disabled={previewing} onClick={async () => {
              try {
                const path = await pickFile({
                  multiple: false,
                  directory: false,
                  title: t("selectAVideoFile"),
                  filters: [{ name: t("videoThumbnails"), extensions: ["mp4", "mkv", "ts", "flv", "mov", "webm", "m4v", "avi", "m3u8", "mts", "m2ts"] }],
                });
                if (typeof path === "string") form.setFieldValue("local_path", path);
              } catch (error) {
                message.error(formatError(error));
              }
            }}>{t("selectFile")}</Button>
          </>
        ) : (
          <>
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
          </>
        )}
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
