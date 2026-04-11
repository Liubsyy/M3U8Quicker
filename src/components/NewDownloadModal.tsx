import { useState, useEffect } from "react";
import { Modal, Form, Input, Button, Space, Radio, Typography, message } from "antd";
import { FolderOpenOutlined } from "@ant-design/icons";
import { open } from "@tauri-apps/plugin-dialog";
import {
  getAppSettings,
  getDefaultDownloadDir,
  getFfmpegStatus,
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
  onSubmit,
}: NewDownloadModalProps) {
  const [form] = Form.useForm();
  const [submitting, setSubmitting] = useState(false);
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
      const mode: DownloadMode = isDirectFileType(initialFileType) ? "direct" : "hls";
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
    message.success("下载已开始");
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
        ? "当前下载包含独立音频或字幕轨。你已开启“合并 mp4”，要在下载完成后自动合成为 mp4，需要先在设置里开启并配置 FFmpeg。"
        : "当前下载包含独立音频或字幕轨。建议先在设置里开启并配置 FFmpeg，后续合成 mp4 会更方便。";

      return await new Promise<boolean>((resolve) => {
        Modal.confirm({
          title: "多轨下载建议开启 FFmpeg",
          content: <Typography.Paragraph style={{ marginBottom: 0 }}>{description}</Typography.Paragraph>,
          okText: "前往设置",
          cancelText: "继续下载",
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
        downloadMode === "direct" ? inferDirectFileTypeFromUrl(url) : "hls";

      if (!fileType) {
        form.setFields([
          {
            name: "url",
            errors: [
              `无法从地址推断文件类型，请使用包含 ${DIRECT_FILE_TYPES.join(
                "/"
              )} 后缀的直链`,
            ],
          },
        ]);
        return;
      }

      setSubmitting(true);
      const nextParams: CreateDownloadParams = {
        url,
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

      await submitDownload(nextParams);
    } catch (e: unknown) {
      if (e && typeof e === "object" && "errorFields" in e) return;
      message.error(`创建下载失败: ${formatCreateDownloadError(e)}`);
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
      message.error("请选择视频轨道");
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
      message.error(`创建下载失败: ${formatCreateDownloadError(error)}`);
    } finally {
      setSubmitting(false);
    }
  };

  const inferredDirectFileType = inferDirectFileTypeFromUrl(watchedUrl);
  const urlLabel = downloadMode === "direct" ? "地址" : "M3U8 地址";
  const supportedDirectTypes = DIRECT_FILE_TYPES.join(" / ");
  const urlPlaceholder =
    downloadMode === "direct"
      ? `https://example.com/video/file.mp4\n支持 ${supportedDirectTypes} 格式`
      : "https://example.com/video/playlist.m3u8";
  const urlRequiredMessage =
    downloadMode === "direct" ? "请输入 Direct 地址" : "请输入 M3U8 地址";
  const urlExtra =
    downloadMode === "direct"
      ? inferredDirectFileType
        ? `文件类型将按地址推断为 ${getFileTypeLabel(inferredDirectFileType)}`
        : undefined
      : undefined;

  return (
    <Modal
      title="新建下载"
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
        <Form.Item label="下载方式">
          <Radio.Group
            value={downloadMode}
            onChange={(event) => {
              setDownloadMode(event.target.value as DownloadMode);
              form.setFields([{ name: "url", errors: [] }]);
            }}
          >
            <Radio.Button value="hls">HLS</Radio.Button>
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
        >
          <Input.TextArea
            placeholder={
              "按行输入，每行一个 header\nreferer:https://www.xx.com\norigin:https://www.xx.com"
            }
            rows={3}
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
      <Modal
        title="选择下载轨道"
        open={Boolean(hlsInspection)}
        onCancel={closeTrackModal}
        onOk={() => {
          void handleConfirmTrackSelection();
        }}
        okText="开始下载"
        cancelText="返回"
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
        已检测到多个视频、音频或字幕，请确认需要下载的轨道。
      </Typography.Text>
      <TrackRadioGroup
        title="视频"
        value={normalizedSelection.video_id}
        options={inspection.video_tracks}
        onChange={(videoId) => {
          onChange(normalizeTrackSelection(inspection, { ...normalizedSelection, video_id: videoId }));
        }}
      />
      {audioTracks.length > 0 ? (
        <TrackRadioGroup
          title="音频"
          value={normalizedSelection.audio_id}
          options={audioTracks}
          onChange={(audioId) => {
            onChange({ ...normalizedSelection, audio_id: audioId });
          }}
        />
      ) : null}
      {subtitleTracks.length > 0 ? (
        <TrackRadioGroup
          title="字幕"
          value={normalizedSelection.subtitle_id ?? "__none__"}
          options={[
            {
              id: "__none__",
              label: "不下载字幕",
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
