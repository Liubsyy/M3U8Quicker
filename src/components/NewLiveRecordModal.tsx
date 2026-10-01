import { t, useTranslation } from "../i18n";
import { useEffect, useState } from "react";
import {
  Button,
  Form,
  Input,
  InputNumber,
  message,
  Modal,
  Select,
  Space,
  Switch,
} from "antd";
import { FolderOpenOutlined } from "@ant-design/icons";
import { open } from "@tauri-apps/plugin-dialog";
import {
  getAppSettings,
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
  split_enabled: boolean;
  split_size_mb?: number | null;
  split_duration_min?: number | null;
}

const MIN_SPLIT_SIZE_MB = 10;
const MIN_SPLIT_DURATION_MIN = 1;

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
  useTranslation();
  const [form] = Form.useForm<FormValues>();
  const [submitting, setSubmitting] = useState(false);
  const [outputDir, setOutputDir] = useState("");
  const [filenameTouched, setFilenameTouched] = useState(false);
  const splitEnabled = Form.useWatch("split_enabled", form) ?? false;

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
      // 分段设置默认取上次录制的选择（保存在全局设置里）。
      getAppSettings()
        .then((settings) => {
          form.setFieldsValue({
            split_enabled: settings.live_split_enabled,
            split_size_mb: settings.live_split_size_mb,
            split_duration_min: settings.live_split_duration_min,
          });
        })
        .catch(() => undefined);
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
        message.error(t("liveStreamUrlIsRequired"));
        return;
      }

      let split: CreateLiveRecordParams["split"];
      if (values.split_enabled) {
        const sizeMb = values.split_size_mb ?? null;
        const durationMin = values.split_duration_min ?? null;
        if (!sizeMb && !durationMin) {
          message.error(t("setAtLeastOneSizeOrDurationLimitForSplit"));
          return;
        }
        split = { size_mb: sizeMb, duration_min: durationMin };
      }

      setSubmitting(true);
      const params: CreateLiveRecordParams = {
        url,
        filename: values.filename?.trim() || undefined,
        output_dir: outputDir || undefined,
        extra_headers: values.extra_headers?.trim() || undefined,
        protocol: values.protocol ?? "flv",
        split,
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
      message.success(t("liveRecordingStarted"));
      onClose();
    } catch (e: unknown) {
      if (e && typeof e === "object" && "errorFields" in e) return;
      message.error(t("failedToStartLiveRecording", { value0: formatError(e) }));
    } finally {
      setSubmitting(false);
    }
  };

  const confirmSwitchToDownload = async (params: CreateLiveRecordParams) => {
    return await new Promise<boolean>((resolve) => {
      Modal.confirm({
        title: t("nonLiveHlsDetected"),
        content: t("thisUrlDoesNotAppearToBeLiveANormal"),
        okText: t("switchToDownload"),
        cancelText: t("continueRecording"),
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
      title={t("newLiveRecording")}
      open={isOpen}
      onCancel={onClose}
      footer={null}
      width={560}
      destroyOnClose
    >
      <Form
        layout="vertical"
        form={form}
        initialValues={{ protocol: "flv", split_enabled: false }}
        onFinish={() => void handleSubmit()}
      >
        <Form.Item
          label={t("liveStreamUrl")}
          name="url"
          rules={[{ required: true, message: t("enterALiveStreamUrl") }]}
        >
          <Input
            placeholder="HTTP-FLV: https://example.com/live/stream.flv，HLS: https://example.com/live/index.m3u8"
            onChange={(e) => handleUrlChange(e.target.value)}
            allowClear
          />
        </Form.Item>
        <Form.Item label={t("protocol")} name="protocol">
          <Select
            options={[
              { value: "flv", label: "HTTP-FLV" },
              { value: "hls", label: "HLS (m3u8)" },
            ]}
          />
        </Form.Item>
        <Form.Item label={t("filenameWithoutExtension")} name="filename">
          <Input
            placeholder={t("leaveBlankToDeriveTheNameFromTheUrl")}
            allowClear
            onChange={() => setFilenameTouched(true)}
          />
        </Form.Item>
        <Form.Item label={t("saveTo")}>
          <Space.Compact style={{ width: "100%" }}>
            <Input value={outputDir} readOnly />
            <Button icon={<FolderOpenOutlined />} onClick={() => void handleSelectDir()}>
              {t("browse")}</Button>
          </Space.Compact>
        </Form.Item>
        <Form.Item label={t("splitRecordingSplitWhenEitherLimitIsReachedLeaveBlank")}>
          <Space align="center" wrap>
            <Form.Item name="split_enabled" valuePropName="checked" noStyle>
              <Switch checkedChildren={t("on")} unCheckedChildren={t("off")} />
            </Form.Item>
            <Form.Item name="split_size_mb" noStyle>
              <InputNumber
                min={MIN_SPLIT_SIZE_MB}
                step={100}
                precision={0}
                disabled={!splitEnabled}
                placeholder={t("bySize")}
                addonAfter="MB"
                style={{ width: 160 }}
              />
            </Form.Item>
            <Form.Item name="split_duration_min" noStyle>
              <InputNumber
                min={MIN_SPLIT_DURATION_MIN}
                step={10}
                precision={0}
                disabled={!splitEnabled}
                placeholder={t("byDuration")}
                addonAfter={t("min")}
                style={{ width: 160 }}
              />
            </Form.Item>
          </Space>
        </Form.Item>
        <Form.Item label={t("additionalHeaders")} name="extra_headers">
          <Input.TextArea
            rows={3}
            placeholder={t("onePerLineInNameValueFormatForExampleReferer")}
          />
        </Form.Item>
        <Form.Item style={{ marginBottom: 0 }}>
          <Space style={{ width: "100%", justifyContent: "flex-end" }}>
            <Button onClick={onClose}>{t("cancel")}</Button>
            <Button type="primary" htmlType="submit" loading={submitting}>
              {t("startRecording")}</Button>
          </Space>
        </Form.Item>
      </Form>
    </Modal>
  );
}

function formatError(e: unknown): string {
  if (!e) return t("unknownError");
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
