import { t, useTranslation } from "../i18n";
import { useState, useEffect } from "react";
import { Modal, Form, Input, Button, Space, Radio, Typography, message } from "antd";
import { FolderOpenOutlined, PictureOutlined } from "@ant-design/icons";
import { open } from "@tauri-apps/plugin-dialog";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  closePreviewSession,
  createPreviewSession,
  getAppSettings,
  getDefaultDownloadDir,
  getFfmpegStatus,
  inspectDashTracks,
  inspectHlsTracks,
  setDefaultDownloadDir,
} from "../services/api";
import {
  deriveFilenameFromUrl,
  DIRECT_FILE_TYPES,
  getFileTypeLabel,
  inferDirectFileTypeFromUrl,
  isDirectFileType,
  type CreateDownloadParams,
  type DownloadMode,
  type FileType,
  type HlsTrackOption,
  type HlsTrackSelection,
  type InspectHlsTracksResult,
} from "../types";

interface NewDownloadModalProps {
  open: boolean;
  initialUrl?: string;
  initialExtraHeaders?: string;
  initialFileType?: FileType;
  resetKey?: number;
  onClose: () => void;
  onOpenFfmpegSettings: () => void;
  onSwitchToLiveRecord: (draft: {
    url: string;
    extraHeaders?: string;
    filename?: string;
    outputDir?: string;
  }) => void;
  onSubmit: (params: CreateDownloadParams) => Promise<void>;
}

export function NewDownloadModal({
  open: isOpen,
  initialUrl,
  initialExtraHeaders,
  initialFileType,
  resetKey,
  onClose,
  onOpenFfmpegSettings,
  onSwitchToLiveRecord,
  onSubmit,
}: NewDownloadModalProps) {
  useTranslation();
  const [form] = Form.useForm();
  const [submitting, setSubmitting] = useState(false);
  const [previewing, setPreviewing] = useState(false);
  const [outputDir, setOutputDir] = useState("");
  const [filenameTouched, setFilenameTouched] = useState(false);
  const [downloadMode, setDownloadMode] = useState<DownloadMode>("hls");
  const [pendingHlsParams, setPendingHlsParams] = useState<CreateDownloadParams | null>(null);
  const [hlsInspection, setHlsInspection] = useState<InspectHlsTracksResult | null>(null);
  const [hlsSelection, setHlsSelection] = useState<HlsTrackSelection>({});
  const watchedUrl = Form.useWatch("url", form) as string | undefined;

  useEffect(() => {
    if (isOpen) {
      getDefaultDownloadDir().then(setOutputDir);
      setFilenameTouched(false);
      setPendingHlsParams(null);
      setHlsInspection(null);
      setHlsSelection({});
      const mode: DownloadMode = isDirectFileType(initialFileType)
        ? "direct"
        : initialFileType === "dash"
          ? "dash"
          : "hls";
      setDownloadMode(mode);
      form.resetFields();
      form.setFieldsValue({
        url: initialUrl || undefined,
        filename: initialUrl ? deriveFilenameFromUrl(initialUrl) || undefined : undefined,
        extra_headers: initialExtraHeaders || undefined,
      });
    }
  }, [form, initialExtraHeaders, initialFileType, initialUrl, isOpen, resetKey]);

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

  const closeTrackModal = () => {
    setPendingHlsParams(null);
    setHlsInspection(null);
    setHlsSelection({});
  };

  const handleUrlChange = (value: string) => {
    if (filenameTouched) return;

    const derived = deriveFilenameFromUrl(value);
    form.setFieldValue("filename", derived || undefined);
  };

  const submitDownload = async (params: CreateDownloadParams) => {
    await onSubmit(params);
    message.success(t("downloadStarted"));
  };

  const confirmSwitchToLiveRecord = async (params: CreateDownloadParams) => {
    return await new Promise<boolean>((resolve) => {
      Modal.confirm({
        title: t("hlsLiveStreamDetected"),
        content: (
          <Typography.Paragraph style={{ marginBottom: 0 }}>
            {t("thisUrlAppearsToBeALiveStreamANormal")}</Typography.Paragraph>
        ),
        okText: t("switchToLiveRecording"),
        cancelText: t("resumeDownload"),
        onOk: () => {
          onSwitchToLiveRecord({
            url: params.url,
            extraHeaders: params.extra_headers,
            filename: params.filename,
            outputDir: params.output_dir,
          });
          resolve(true);
        },
        onCancel: () => resolve(false),
      });
    });
  };

  const ensureMultiTrackFfmpegReady = async (
    inspection: InspectHlsTracksResult,
    selection: HlsTrackSelection
  ) => {
    if (!willCreateMultiTrackBundle(inspection, selection)) {
      return true;
    }

    try {
      const [settings, ffmpegStatus] = await Promise.all([
        getAppSettings(),
        getFfmpegStatus(),
      ]);

      if (settings.ffmpeg_enabled && ffmpegStatus.kind === "installed") {
        return true;
      }

      const description = settings.convert_to_mp4
        ? t("thisDownloadHasSeparateAudioOrSubtitleTracksMergeTo")
        : t("thisDownloadHasSeparateAudioOrSubtitleTracksEnableAnd");

      return await new Promise<boolean>((resolve) => {
        Modal.confirm({
          title: t("ffmpegRecommendedForMultiTrackDownloads"),
          content: <Typography.Paragraph style={{ marginBottom: 0 }}>{description}</Typography.Paragraph>,
          okText: t("goToSettings"),
          cancelText: t("resumeDownload"),
          onOk: () => {
            onOpenFfmpegSettings();
            resolve(false);
          },
          onCancel: () => resolve(true),
        });
      });
    } catch {
      return true;
    }
  };

  const handleSubmit = async () => {
    try {
      const values = await form.validateFields();
      const url = values.url.trim();
      const fileType =
        downloadMode === "direct"
          ? inferDirectFileTypeFromUrl(url) ?? "mp4"
          : downloadMode;
      const isInlineDashJson = downloadMode === "dash" && url.startsWith("{");

      setSubmitting(true);
      const nextParams: CreateDownloadParams = {
        url: isInlineDashJson ? "inline-dash-json" : url,
        source_kind: isInlineDashJson ? "inline_dash_json" : "url",
        source_text: isInlineDashJson ? url : undefined,
        filename: values.filename?.trim() || undefined,
        output_dir: outputDir || undefined,
        extra_headers: values.extra_headers?.trim() || undefined,
        download_mode: downloadMode,
        file_type: fileType,
      };

      if (downloadMode === "hls") {
        const inspection = await inspectHlsTracks({
          url,
          extra_headers: nextParams.extra_headers,
        });
        if (inspection.is_live && (await confirmSwitchToLiveRecord(nextParams))) {
          return;
        }

        if (inspection.kind === "master" && inspection.requires_selection) {
          setPendingHlsParams(nextParams);
          setHlsInspection(inspection);
          setHlsSelection(normalizeTrackSelection(inspection, inspection.default_selection));
          return;
        }

        const normalizedSelection =
          inspection.kind === "master"
            ? normalizeTrackSelection(inspection, inspection.default_selection)
            : undefined;
        if (
          normalizedSelection &&
          !(await ensureMultiTrackFfmpegReady(inspection, normalizedSelection))
        ) {
          return;
        }

        await submitDownload({
          ...nextParams,
          hls_selection: normalizedSelection,
        });
        return;
      }

      if (downloadMode === "dash") {
        const inspection = await inspectDashTracks({
          url: isInlineDashJson ? "inline-dash-json" : url,
          source_kind: isInlineDashJson ? "inline_dash_json" : "url",
          source_text: isInlineDashJson ? url : undefined,
          extra_headers: nextParams.extra_headers,
        });
        if (inspection.requires_selection) {
          setPendingHlsParams(nextParams);
          setHlsInspection(inspection);
          setHlsSelection(normalizeTrackSelection(inspection, inspection.default_selection));
          return;
        }

        const normalizedSelection = normalizeTrackSelection(
          inspection,
          inspection.default_selection
        );
        if (!(await ensureMultiTrackFfmpegReady(inspection, normalizedSelection))) {
          return;
        }

        await submitDownload({
          ...nextParams,
          hls_selection: normalizedSelection,
        });
        return;
      }

      await submitDownload(nextParams);
    } catch (e: unknown) {
      if (e && typeof e === "object" && "errorFields" in e) return;
      message.error(t("failedToCreateDownload", { value0: formatCreateDownloadError(e) }));
    } finally {
      setSubmitting(false);
    }
  };

  const handleConfirmTrackSelection = async () => {
    if (!pendingHlsParams || !hlsInspection) {
      return;
    }

    const normalizedSelection = normalizeTrackSelection(hlsInspection, hlsSelection);
    if (!normalizedSelection.video_id) {
      message.error(t("selectAVideoTrack"));
      return;
    }

    try {
      setSubmitting(true);
      if (!(await ensureMultiTrackFfmpegReady(hlsInspection, normalizedSelection))) {
        return;
      }
      await submitDownload({
        ...pendingHlsParams,
        hls_selection: normalizedSelection,
      });
      closeTrackModal();
    } catch (error) {
      message.error(t("failedToCreateDownload", { value0: formatCreateDownloadError(error) }));
    } finally {
      setSubmitting(false);
    }
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

  const handlePreview = async () => {
    try {
      const values = await form.validateFields(["url"]);
      const rawUrl = (values.url as string | undefined)?.trim();
      if (!rawUrl) {
        return;
      }
      const extraHeaders =
        (form.getFieldValue("extra_headers") as string | undefined)?.trim() ||
        undefined;
      const isInlineDashJson =
        downloadMode === "dash" && rawUrl.startsWith("{");
      const url = isInlineDashJson ? "inline-dash-json" : rawUrl;
      const sourceKind = isInlineDashJson ? "inline_dash_json" : undefined;
      const sourceText = isInlineDashJson ? rawUrl : undefined;

      setPreviewing(true);
      if (!(await ensurePreviewFfmpegReady())) {
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
    } catch (e: unknown) {
      if (e && typeof e === "object" && "errorFields" in e) return;
      message.error(t("failedToGeneratePreview", { value0: formatCreateDownloadError(e) }));
    } finally {
      setPreviewing(false);
    }
  };

  const inferredDirectFileType = inferDirectFileTypeFromUrl(watchedUrl);
  const urlLabel =
    downloadMode === "direct" ? t("url") : downloadMode === "dash" ? t("dashUrlJson") : t("m3u8Url");
  const supportedDirectTypes = DIRECT_FILE_TYPES.join(" / ");
  const urlPlaceholder =
    downloadMode === "direct"
      ? t("httpsExampleComVideoFileMp4SupportedFormats", { value0: supportedDirectTypes })
      : downloadMode === "dash"
        ? t("httpsExampleComVideoManifestMpdOrPasteM3u8quickerDash")
        : "https://example.com/video/playlist.m3u8";
  const urlRequiredMessage =
    downloadMode === "direct"
      ? t("enterADirectDownloadUrl")
      : downloadMode === "dash"
        ? t("enterADashUrlOrJson")
        : t("enterAnM3u8Url");
  const urlExtra =
    downloadMode === "direct"
      ? inferredDirectFileType
        ? t("detectedFileType", { value0: getFileTypeLabel(inferredDirectFileType) })
        : t("cannotDetectFileTypeMp4WillBeUsed")
      : undefined;

  return (
    <Modal
      title={t("newDownload")}
      open={isOpen}
      onCancel={() => {
        closeTrackModal();
        onClose();
      }}
      footer={null}
      destroyOnClose
      width={520}
    >
      <Form
        form={form}
        layout="vertical"
        className="new-download-form"
        onFinish={handleSubmit}
      >
        <Form.Item label={t("downloadType")}>
          <Radio.Group
            value={downloadMode}
            onChange={(event) => {
              setDownloadMode(event.target.value as DownloadMode);
              form.setFields([{ name: "url", errors: [] }]);
            }}
          >
            <Radio.Button value="hls">HLS</Radio.Button>
            <Radio.Button value="dash">DASH</Radio.Button>
            <Radio.Button value="direct">Direct</Radio.Button>
          </Radio.Group>
        </Form.Item>
        <Form.Item
          name="url"
          label={urlLabel}
          extra={urlExtra}
          rules={[{ required: true, message: urlRequiredMessage }]}
        >
          <Input.TextArea
            placeholder={urlPlaceholder}
            rows={3}
            autoFocus
            onChange={(event) => handleUrlChange(event.target.value)}
          />
        </Form.Item>
        <Form.Item name="filename" label={t("filenameOptional")}>
          <Input
            placeholder={t("leaveBlankToDeriveTheNameFromTheUrlTitle")}
            onChange={(event) => {
              const value = event.target.value;
              setFilenameTouched(Boolean(value.trim()));
            }}
          />
        </Form.Item>
        <Form.Item
          name="extra_headers"
          label={t("additionalHeaders")}
        >
          <Input.TextArea
            placeholder={
              t("oneHeaderPerLineRefererHttpsExampleComOriginHttps")
            }
            rows={3}
          />
        </Form.Item>
        <Form.Item label={t("saveTo")}>
          <Space.Compact style={{ width: "100%" }}>
            <Input value={outputDir} readOnly style={{ flex: 1 }} />
            <Button icon={<FolderOpenOutlined />} onClick={handleSelectDir}>
              {t("browse")}</Button>
          </Space.Compact>
        </Form.Item>
        <Form.Item style={{ marginBottom: 0, textAlign: "right" }}>
          <Space>
            <Button onClick={onClose}>{t("cancel")}</Button>
            <Button
              color="cyan"
              variant="solid"
              icon={<PictureOutlined />}
              onClick={handlePreview}
              loading={previewing}
            >
              {t("preview")}</Button>
            <Button type="primary" htmlType="submit" loading={submitting}>
              {t("startDownload")}</Button>
          </Space>
        </Form.Item>
      </Form>
      <Modal
        title={t("selectDownloadTracks")}
        open={Boolean(hlsInspection)}
        onCancel={closeTrackModal}
        onOk={() => {
          void handleConfirmTrackSelection();
        }}
        okText={t("startDownload")}
        cancelText={t("back")}
        confirmLoading={submitting}
        destroyOnClose
        maskClosable={false}
      >
        {hlsInspection ? (
          <HlsTrackSelectionContent
            inspection={hlsInspection}
            selection={hlsSelection}
            onChange={setHlsSelection}
          />
        ) : null}
      </Modal>
    </Modal>
  );
}

interface HlsTrackSelectionContentProps {
  inspection: InspectHlsTracksResult;
  selection: HlsTrackSelection;
  onChange: (selection: HlsTrackSelection) => void;
}

function HlsTrackSelectionContent({
  inspection,
  selection,
  onChange,
}: HlsTrackSelectionContentProps) {
  useTranslation();
  const normalizedSelection = normalizeTrackSelection(inspection, selection);
  const selectedVideo = inspection.video_tracks.find(
    (track) => track.id === normalizedSelection.video_id
  );
  const audioTracks = filterTracksForSelectedVideo(
    inspection.audio_tracks,
    selectedVideo?.audio_group_id
  );
  const subtitleTracks = filterTracksForSelectedVideo(
    inspection.subtitle_tracks,
    selectedVideo?.subtitle_group_id
  );

  return (
    <div style={{ display: "grid", gap: 16 }}>
      <Typography.Text type="secondary">
        {t("multipleVideoAudioOrSubtitleTracksDetectedSelectTheTracks")}</Typography.Text>
      <TrackRadioGroup
        title={t("video")}
        value={normalizedSelection.video_id}
        options={inspection.video_tracks}
        onChange={(videoId) => {
          onChange(normalizeTrackSelection(inspection, { ...normalizedSelection, video_id: videoId }));
        }}
      />
      {audioTracks.length > 0 ? (
        <TrackRadioGroup
          title={t("audio")}
          value={normalizedSelection.audio_id}
          options={audioTracks}
          onChange={(audioId) => {
            onChange({ ...normalizedSelection, audio_id: audioId });
          }}
        />
      ) : null}
      {subtitleTracks.length > 0 ? (
        <TrackRadioGroup
          title={t("subtitles")}
          value={normalizedSelection.subtitle_id ?? "__none__"}
          options={[
            {
              id: "__none__",
              label: t("noSubtitles"),
              track_type: "subtitle",
              name: null,
              language: null,
              group_id: null,
              audio_group_id: null,
              subtitle_group_id: null,
              bandwidth: null,
              resolution: null,
              codecs: null,
              is_default: false,
              is_autoselect: false,
              is_forced: false,
            },
            ...subtitleTracks,
          ]}
          onChange={(subtitleId) => {
            onChange({
              ...normalizedSelection,
              subtitle_id: subtitleId === "__none__" ? undefined : subtitleId,
            });
          }}
        />
      ) : null}
    </div>
  );
}

interface TrackRadioGroupProps {
  title: string;
  value?: string;
  options: HlsTrackOption[];
  onChange: (value: string) => void;
}

function TrackRadioGroup({ title, value, options, onChange }: TrackRadioGroupProps) {
  useTranslation();
  return (
    <div style={{ display: "grid", gap: 8 }}>
      <Typography.Text strong>{title}</Typography.Text>
      <Radio.Group
        value={value}
        onChange={(event) => onChange(event.target.value as string)}
        style={{ display: "grid", gap: 8 }}
      >
        {options.map((option) => (
          <Radio
            key={option.id}
            value={option.id}
            style={{
              display: "flex",
              alignItems: "flex-start",
              marginInlineStart: 0,
              padding: "10px 12px",
              border: "1px solid #d9d9d9",
              borderRadius: 8,
            }}
          >
            <span>{option.label}</span>
          </Radio>
        ))}
      </Radio.Group>
    </div>
  );
}

function filterTracksForSelectedVideo(
  tracks: HlsTrackOption[],
  groupId: string | null | undefined
) {
  if (!groupId) {
    return [];
  }

  return tracks.filter((track) => track.group_id === groupId);
}

function pickDefaultAudioTrack(tracks: HlsTrackOption[]) {
  return (
    tracks.find((track) => track.is_default) ??
    tracks.find((track) => track.is_autoselect) ??
    tracks[0]
  );
}

function normalizeTrackSelection(
  inspection: InspectHlsTracksResult,
  selection: HlsTrackSelection
): HlsTrackSelection {
  const fallbackVideo = inspection.default_selection.video_id ?? inspection.video_tracks[0]?.id;
  const video_id =
    selection.video_id && inspection.video_tracks.some((track) => track.id === selection.video_id)
      ? selection.video_id
      : fallbackVideo;
  const selectedVideo = inspection.video_tracks.find((track) => track.id === video_id);
  const audioTracks = filterTracksForSelectedVideo(
    inspection.audio_tracks,
    selectedVideo?.audio_group_id
  );
  const subtitleTracks = filterTracksForSelectedVideo(
    inspection.subtitle_tracks,
    selectedVideo?.subtitle_group_id
  );

  const audio_id =
    audioTracks.length === 0
      ? undefined
      : selection.audio_id && audioTracks.some((track) => track.id === selection.audio_id)
        ? selection.audio_id
        : pickDefaultAudioTrack(audioTracks)?.id;
  const subtitle_id =
    selection.subtitle_id &&
    subtitleTracks.some((track) => track.id === selection.subtitle_id)
      ? selection.subtitle_id
      : undefined;

  return {
    video_id,
    audio_id,
    subtitle_id,
  };
}

function willCreateMultiTrackBundle(
  inspection: InspectHlsTracksResult,
  selection: HlsTrackSelection
) {
  if (inspection.kind !== "master") {
    return false;
  }

  const normalizedSelection = normalizeTrackSelection(inspection, selection);
  const selectedVideo = inspection.video_tracks.find(
    (track) => track.id === normalizedSelection.video_id
  );

  if (!selectedVideo) {
    return false;
  }

  const audioTracks = filterTracksForSelectedVideo(
    inspection.audio_tracks,
    selectedVideo.audio_group_id
  );
  const subtitleTracks = filterTracksForSelectedVideo(
    inspection.subtitle_tracks,
    selectedVideo.subtitle_group_id
  );

  const hasSelectedAudio =
    Boolean(normalizedSelection.audio_id) &&
    audioTracks.some((track) => track.id === normalizedSelection.audio_id);
  const hasSelectedSubtitle =
    Boolean(normalizedSelection.subtitle_id) &&
    subtitleTracks.some((track) => track.id === normalizedSelection.subtitle_id);

  return hasSelectedAudio || hasSelectedSubtitle;
}

function formatCreateDownloadError(error: unknown) {
  const text = String(error ?? "").trim();
  if (!text) {
    return t("unknownError");
  }

  const normalized = text.replace(
    /^(Invalid input|M3U8 parse error|Network error|IO error|URL parse error|Decryption error|Conversion error):\s*/i,
    ""
  );

  if (/^relative URL without a base$/i.test(normalized)) {
    return t("enterACompleteHttpOrHttpsUrl");
  }

  return normalized;
}
