import { t, useTranslation } from "../i18n";
import { useEffect, useState } from "react";
import {
  Button,
  Descriptions,
  Divider,
  Form,
  Input,
  Modal,
  Radio,
  Select,
  Space,
  Typography,
  message,
} from "antd";
import {
  ArrowDownOutlined,
  ArrowUpOutlined,
  ApartmentOutlined,
  DeleteOutlined,
  FileOutlined,
  FileSyncOutlined,
  FileSearchOutlined,
  FolderOpenOutlined,
  RetweetOutlined,
  MergeCellsOutlined,
  ScissorOutlined,
  SwapOutlined,
} from "@ant-design/icons";
import { open as pickDialogPath, save } from "@tauri-apps/plugin-dialog";
import {
  analyzeMediaFile,
  clipVideoFile,
  convertLocalM3u8ToMp4File,
  convertMultiTrackHlsToMp4Dir,
  convertMediaFile,
  convertTsToMp4File,
  mergeVideoFiles,
  mergeTsFiles,
  transcodeMediaFile,
} from "../services/api";
import type { MediaAnalysisResult } from "../types";
import { VideoClipPicker, type ClipRange } from "./VideoClipPicker";

export type ToolAction =
  | "merge-ts"
  | "ts-to-mp4"
  | "local-m3u8-to-mp4"
  | "merge-video"
  | "clip-video"
  | "format-convert"
  | "codec-convert"
  | "analyze-media"
  | "multi-track-hls-to-mp4"
  | "install-chrome-extension"
  | "install-edge-extension"
  | "install-firefox-extension";

type ClipMode = "fast" | "precise";

const CLIP_MODE_OPTIONS = (): Array<{ value: ClipMode; label: string }> => ([
  { value: "fast", label: t("fastNoReEncoding") },
  { value: "precise", label: t("preciseReEncode") },
]);

type ConvertFormat = "mp4" | "mkv" | "mov" | "mp3" | "m4a" | "wav";
type ConvertMode = "quick" | "compatible";
type MergeVideoMode = "fast" | "compatible";
type CodecOutputFormat = "mp4" | "mkv" | "mov";
type VideoCodec = "h264" | "h265" | "vp9" | "copy";
type AudioCodec = "aac" | "mp3" | "opus" | "copy";

const CONVERT_FORMAT_OPTIONS: Array<{ value: ConvertFormat; label: string }> = [
  { value: "mp4", label: "MP4" },
  { value: "mkv", label: "MKV" },
  { value: "mov", label: "MOV" },
  { value: "mp3", label: "MP3" },
  { value: "m4a", label: "M4A" },
  { value: "wav", label: "WAV" },
];

const CONVERT_MODE_OPTIONS = (): Array<{ value: ConvertMode; label: string }> => ([
  { value: "quick", label: t("fastConversion") },
  { value: "compatible", label: t("compatibleConversion") },
]);

const MERGE_VIDEO_MODE_OPTIONS = (): Array<{ value: MergeVideoMode; label: string }> => ([
  { value: "fast", label: t("fastMerge") },
  { value: "compatible", label: t("compatibleMerge") },
]);

const CODEC_OUTPUT_FORMAT_OPTIONS: Array<{ value: CodecOutputFormat; label: string }> = [
  { value: "mp4", label: "MP4" },
  { value: "mkv", label: "MKV" },
  { value: "mov", label: "MOV" },
];

const VIDEO_CODEC_OPTIONS_BY_FORMAT = (): Record<
  CodecOutputFormat,
  Array<{ value: VideoCodec; label: string }>
> => ({
  mp4: [
    { value: "h264", label: "H.264" },
    { value: "h265", label: "H.265" },
    { value: "copy", label: t("copyOriginalVideoCodec") },
  ],
  mkv: [
    { value: "h264", label: "H.264" },
    { value: "h265", label: "H.265" },
    { value: "vp9", label: "VP9" },
    { value: "copy", label: t("copyOriginalVideoCodec") },
  ],
  mov: [
    { value: "h264", label: "H.264" },
    { value: "h265", label: "H.265" },
    { value: "copy", label: t("copyOriginalVideoCodec") },
  ],
});

const AUDIO_CODEC_OPTIONS_BY_FORMAT = (): Record<
  CodecOutputFormat,
  Array<{ value: AudioCodec; label: string }>
> => ({
  mp4: [
    { value: "aac", label: "AAC" },
    { value: "mp3", label: "MP3" },
    { value: "copy", label: t("copyOriginalAudioCodec") },
  ],
  mkv: [
    { value: "aac", label: "AAC" },
    { value: "mp3", label: "MP3" },
    { value: "opus", label: "Opus" },
    { value: "copy", label: t("copyOriginalAudioCodec") },
  ],
  mov: [
    { value: "aac", label: "AAC" },
    { value: "copy", label: t("copyOriginalAudioCodec") },
  ],
});

interface ToolsModalProps {
  open: boolean;
  tool: ToolAction | null;
  initialInputPath?: string;
  onClose: () => void;
}

export function ToolsModal({ open, tool, initialInputPath, onClose }: ToolsModalProps) {
  useTranslation();
  const [form] = Form.useForm();
  const [submitting, setSubmitting] = useState(false);
  const [analysisResult, setAnalysisResult] = useState<MediaAnalysisResult | null>(null);
  const [clipStatus, setClipStatus] = useState<{ duration: number; loadFailed: boolean }>(
    { duration: 0, loadFailed: false }
  );
  const codecOutputFormat = Form.useWatch("output_format", form) as
    | CodecOutputFormat
    | undefined;
  const mergeVideoInputPaths = Form.useWatch("input_paths", form) as string[] | undefined;
  const clipInputPath = Form.useWatch("input_path", form) as string | undefined;

  const title = (() => {
    if (tool === "merge-ts") {
      return (
        <Space size={8}>
          <MergeCellsOutlined />
          <span>{t("mergeTs")}</span>
        </Space>
      );
    }

    if (tool === "ts-to-mp4") {
      return (
        <Space size={8}>
          <SwapOutlined />
          <span>{t("tsToMp4")}</span>
        </Space>
      );
    }

    if (tool === "local-m3u8-to-mp4") {
      return (
        <Space size={8}>
          <FileSyncOutlined />
          <span>{t("localM3u8ToMp4")}</span>
        </Space>
      );
    }

    if (tool === "merge-video") {
      return (
        <Space size={8}>
          <MergeCellsOutlined />
          <span>{t("mergeVideos")}</span>
        </Space>
      );
    }

    if (tool === "clip-video") {
      return (
        <Space size={8}>
          <ScissorOutlined />
          <span>{t("clipVideo")}</span>
        </Space>
      );
    }

    if (tool === "format-convert") {
      return (
        <Space size={8}>
          <SwapOutlined />
          <span>{t("convertFormat")}</span>
        </Space>
      );
    }

    if (tool === "codec-convert") {
      return (
        <Space size={8}>
          <RetweetOutlined />
          <span>{t("transcode")}</span>
        </Space>
      );
    }

    if (tool === "analyze-media") {
      return (
        <Space size={8}>
          <FileSearchOutlined />
          <span>{t("analyzeVideo")}</span>
        </Space>
      );
    }

    if (tool === "multi-track-hls-to-mp4") {
      return (
        <Space size={8}>
          <ApartmentOutlined />
          <span>{t("multiTrackHlsToMp4")}</span>
        </Space>
      );
    }

    return t("tools");
  })();

  useEffect(() => {
    if (!open) return;
    form.resetFields();
    if (tool === "analyze-media" && initialInputPath) {
      form.setFieldValue("input_path", initialInputPath);
    }
    setAnalysisResult(null);
    setClipStatus({ duration: 0, loadFailed: false });
    if (tool === "format-convert") {
      form.setFieldValue("target_format", "mp4");
      form.setFieldValue("convert_mode", "quick");
    }
    if (tool === "codec-convert") {
      form.setFieldValue("output_format", "mp4");
      form.setFieldValue("video_codec", "h264");
      form.setFieldValue("audio_codec", "aac");
    }
    if (tool === "merge-video") {
      form.setFieldValue("merge_mode", "fast");
    }
    if (tool === "clip-video") {
      form.setFieldValue("clip_mode", "fast");
    }
  }, [form, open, tool, initialInputPath]);

  const handlePickInput = async () => {
    if (tool === "merge-ts" || tool === "multi-track-hls-to-mp4") {
      const selected = await pickDialogPath({
        multiple: false,
        directory: true,
      });

      if (!selected) return;
      const inputDir = selected as string;
      form.setFieldValue("input_path", inputDir);
      if (!form.getFieldValue("output_path")) {
        form.setFieldValue(
          "output_path",
          tool === "merge-ts"
            ? buildMergedOutputPath(inputDir)
            : buildMultiTrackMp4OutputPath(inputDir)
        );
      }
      return;
    }

    if (tool === "merge-video") {
      const selected = await pickDialogPath({
        multiple: true,
        directory: false,
        filters: [
          {
            name: t("videoFiles"),
            extensions: ["mp4", "mkv", "mov", "webm", "avi", "wmv", "flv", "m4v", "ts"],
          },
        ],
      });

      if (!selected) return;
      const inputPaths = Array.isArray(selected) ? selected : [selected as string];
      form.setFieldValue("input_paths", inputPaths);
      if (!form.getFieldValue("output_path")) {
        form.setFieldValue("output_path", buildMergedVideoOutputPath(inputPaths));
      }
      return;
    }

    if (
      tool === "ts-to-mp4" ||
      tool === "local-m3u8-to-mp4" ||
      tool === "analyze-media" ||
      tool === "codec-convert" ||
      tool === "clip-video"
    ) {
      const selected = await pickDialogPath({
        multiple: false,
        directory: false,
        filters:
          tool === "ts-to-mp4"
            ? [{ name: t("tsFiles"), extensions: ["ts"] }]
            : tool === "local-m3u8-to-mp4"
              ? [{ name: t("m3u8Files"), extensions: ["m3u8"] }]
            : tool === "clip-video"
              ? [{ name: t("previewableVideos"), extensions: ["mp4", "m4v", "mov", "webm"] }]
              : undefined,
      });

      if (!selected) return;
      const inputPath = selected as string;
      form.setFieldValue("input_path", inputPath);
      if (tool === "analyze-media") {
        setAnalysisResult(null);
        return;
      }
      if (tool === "codec-convert") {
        const outputFormat =
          (form.getFieldValue("output_format") as CodecOutputFormat | undefined) ?? "mp4";
        form.setFieldValue("output_path", buildConvertedOutputPath(inputPath, outputFormat));
        return;
      }
      if (tool === "local-m3u8-to-mp4") {
        if (!form.getFieldValue("output_path")) {
          form.setFieldValue("output_path", buildLocalM3u8Mp4OutputPath(inputPath));
        }
        return;
      }
      if (tool === "clip-video") {
        form.setFieldValue("clip_range", undefined);
        setClipStatus({ duration: 0, loadFailed: false });
        form.setFieldValue("output_path", buildClipVideoOutputPath(inputPath));
        return;
      }
      if (!form.getFieldValue("output_path")) {
        form.setFieldValue("output_path", buildMp4OutputPath(inputPath));
      }
      return;
    }

    if (tool === "format-convert") {
      const selected = await pickDialogPath({
        multiple: false,
        directory: false,
      });

      if (!selected) return;
      const inputPath = selected as string;
      const targetFormat = (form.getFieldValue("target_format") as ConvertFormat | undefined) ?? "mp4";
      form.setFieldValue("input_path", inputPath);
      form.setFieldValue("output_path", buildConvertedOutputPath(inputPath, targetFormat));
    }
  };

  const handlePickOutput = async () => {
    const currentOutput = form.getFieldValue("output_path") as string | undefined;
    const targetFormat = (form.getFieldValue("target_format") as ConvertFormat | undefined) ?? "mp4";
    const outputFormat =
      (form.getFieldValue("output_format") as CodecOutputFormat | undefined) ?? "mp4";
    const selected = await save({
      defaultPath: currentOutput,
      filters:
        tool === "merge-ts"
          ? [{ name: t("tsFiles"), extensions: ["ts"] }]
          : tool === "merge-video"
            ? [{ name: t("mp4Files"), extensions: ["mp4"] }]
          : tool === "codec-convert"
            ? [{ name: t("files", { value0: outputFormat.toUpperCase() }), extensions: [outputFormat] }]
          : tool === "format-convert"
            ? [{ name: t("files", { value0: targetFormat.toUpperCase() }), extensions: [targetFormat] }]
          : tool === "clip-video"
            ? [{ name: t("mp4Files"), extensions: ["mp4"] }]
            : [{ name: t("mp4Files"), extensions: ["mp4"] }],
    });

    if (selected) {
      form.setFieldValue("output_path", selected);
    }
  };

  const handleSubmit = async () => {
    if (!tool) return;

    try {
      const values = await form.validateFields();
      setSubmitting(true);

      if (tool === "merge-ts") {
        const requestedOutput = values.output_path.trim();
        const savedPath = await mergeTsFiles(values.input_path.trim(), requestedOutput);
        message.success(
          savedPath === requestedOutput
            ? t("tsFilesMerged")
            : t("tsFilesMergedAndSavedAs", { value0: getPathName(savedPath) })
        );
      } else if (tool === "ts-to-mp4") {
        const requestedOutput = values.output_path.trim();
        const savedPath = await convertTsToMp4File(values.input_path.trim(), requestedOutput);
        message.success(
          savedPath === requestedOutput
            ? t("mp4CreatedOriginalTsFileKept")
            : t("mp4CreatedAndSavedAsOriginalTsFileKept", { value0: getPathName(savedPath) })
        );
      } else if (tool === "local-m3u8-to-mp4") {
        const requestedOutput = values.output_path.trim();
        const savedPath = await convertLocalM3u8ToMp4File(
          values.input_path.trim(),
          requestedOutput
        );
        message.success(
          savedPath === requestedOutput
            ? t("m3u8ConvertedToMp4OriginalFilesKept")
            : t("m3u8ConvertedToMp4AndSavedAsOriginalFilesKept", { value0: getPathName(savedPath) })
        );
      } else if (tool === "merge-video") {
        const inputPaths =
          (form.getFieldValue("input_paths") as string[] | undefined) ?? [];
        if (inputPaths.length < 2) {
          message.error(t("selectAtLeastTwoVideoFiles"));
          return;
        }
        const requestedOutput = values.output_path.trim();
        const mergeMode = (values.merge_mode as MergeVideoMode | undefined) ?? "fast";
        const savedPath = await mergeVideoFiles(inputPaths, requestedOutput, mergeMode);
        message.success(
          savedPath === requestedOutput
            ? t("videosMergedOriginalFilesKept")
            : t("videosMergedAndSavedAsOriginalFilesKept", { value0: getPathName(savedPath) })
        );
      } else if (tool === "clip-video") {
        if (clipStatus.loadFailed) {
          message.error(t("thisFileCannotBePreviewedConvertItToMp4Before"));
          return;
        }
        const range = values.clip_range as ClipRange | undefined;
        if (!range || !(range.end - range.start >= 0.05)) {
          message.error(t("selectAValidClipRange"));
          return;
        }
        const requestedOutput = values.output_path.trim();
        const savedPath = await clipVideoFile(
          values.input_path.trim(),
          requestedOutput,
          range.start,
          range.end,
          (values.clip_mode as ClipMode | undefined) ?? "fast"
        );
        message.success(
          savedPath === requestedOutput
            ? t("videoClippedOriginalFileKept")
            : t("videoClippedAndSavedAsOriginalFileKept", { value0: getPathName(savedPath) })
        );
      } else if (tool === "format-convert") {
        const requestedOutput = values.output_path.trim();
        const savedPath = await convertMediaFile(
          values.input_path.trim(),
          requestedOutput,
          values.target_format,
          values.convert_mode
        );
        const formatLabel = String(values.target_format).toUpperCase();
        message.success(
          savedPath === requestedOutput
            ? t("createdOriginalFileKept", { value0: formatLabel })
            : t("createdAndSavedAsOriginalFileKept", { value0: formatLabel, value1: getPathName(savedPath) })
        );
      } else if (tool === "codec-convert") {
        const requestedOutput = values.output_path.trim();
        const savedPath = await transcodeMediaFile(
          values.input_path.trim(),
          requestedOutput,
          values.output_format,
          values.video_codec,
          values.audio_codec
        );
        const formatLabel = String(values.output_format).toUpperCase();
        message.success(
          savedPath === requestedOutput
            ? t("transcodingCompletedOriginalFileKept", { value0: formatLabel })
            : t("transcodingCompletedAndSavedAsOriginalFileKept", { value0: formatLabel, value1: getPathName(savedPath) })
        );
      } else if (tool === "analyze-media") {
        const result = await analyzeMediaFile(values.input_path.trim());
        setAnalysisResult(result);
        message.success(t("videoAnalysisCompleted"));
      } else {
        const requestedOutput = values.output_path.trim();
        const savedPath = await convertMultiTrackHlsToMp4Dir(
          values.input_path.trim(),
          requestedOutput
        );
        message.success(
          savedPath === requestedOutput
            ? t("multiTrackHlsConvertedToMp4OriginalFolderKept")
            : t("multiTrackHlsConvertedToMp4AndSavedAsOriginal", { value0: getPathName(savedPath) })
        );
      }

      if (tool !== "analyze-media") {
        onClose();
      }
    } catch (error: unknown) {
      if (error && typeof error === "object" && "errorFields" in error) return;
      message.error(t("toolFailed", { value0: formatToolError(error) }));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Modal
      title={title}
      open={open}
      onCancel={onClose}
      onOk={() => void handleSubmit()}
      okText={tool === "analyze-media" ? t("startAnalysis") : t("startProcessing")}
      cancelText={t("cancel")}
      confirmLoading={submitting}
      destroyOnClose
      okButtonProps={{
        disabled:
          tool === "clip-video" && (clipStatus.loadFailed || clipStatus.duration <= 0),
      }}
      width={
        tool === "analyze-media"
          ? 760
          : tool === "merge-video"
            ? 720
          : tool === "clip-video"
            ? 760
            : 520
      }
    >
      <Form form={form} layout="vertical">
        {tool === "merge-video" ? (
          <Form.Item label={t("videoFiles")} required>
            <Space direction="vertical" size={8} style={{ width: "100%" }}>
              <Button icon={<FileOutlined />} onClick={() => void handlePickInput()}>
                {t("selectMultipleVideos")}</Button>
              {(mergeVideoInputPaths ?? []).length > 0 ? (
                <Space direction="vertical" size={8} style={{ width: "100%" }}>
                  {(mergeVideoInputPaths ?? []).map((path, index, list) => (
                      <Space.Compact key={`${path}-${index}`} style={{ width: "100%" }}>
                        <Input readOnly value={`${index + 1}. ${path}`} />
                        <Button
                          disabled={index === 0}
                          icon={<ArrowUpOutlined />}
                          onClick={() => {
                            const next = [...list];
                            [next[index - 1], next[index]] = [next[index], next[index - 1]];
                            form.setFieldValue("input_paths", next);
                          }}
                        />
                        <Button
                          disabled={index === list.length - 1}
                          icon={<ArrowDownOutlined />}
                          onClick={() => {
                            const next = [...list];
                            [next[index], next[index + 1]] = [next[index + 1], next[index]];
                            form.setFieldValue("input_paths", next);
                          }}
                        />
                        <Button
                          danger
                          icon={<DeleteOutlined />}
                          onClick={() => {
                            const next = list.filter((_, itemIndex) => itemIndex !== index);
                            form.setFieldValue("input_paths", next);
                          }}
                        />
                      </Space.Compact>
                    )
                  )}
                </Space>
              ) : (
                <Input.TextArea
                  readOnly
                  autoSize={{ minRows: 4, maxRows: 6 }}
                  placeholder={t("selectAtLeastTwoVideosToMerge")}
                />
              )}
            </Space>
          </Form.Item>
        ) : (
          <Form.Item
            label={
              tool === "merge-ts"
                ? t("tsFolder")
                : tool === "ts-to-mp4"
                  ? t("tsFiles")
                  : tool === "local-m3u8-to-mp4"
                    ? t("m3u8Files")
                  : tool === "format-convert"
                    ? t("mediaFile")
                    : tool === "codec-convert"
                      ? t("mediaFile")
                    : tool === "clip-video"
                      ? t("videoFiles")
                    : tool === "analyze-media"
                      ? t("videoFiles")
                      : t("multiTrackHlsFolder")
            }
            required
          >
            <Space.Compact style={{ width: "100%" }}>
              <Form.Item
                name="input_path"
                noStyle
                rules={[
                  {
                    required: true,
                    message:
                      tool === "merge-ts"
                        ? t("selectATsFolder")
                        : tool === "ts-to-mp4"
                          ? t("selectATsFile")
                          : tool === "local-m3u8-to-mp4"
                            ? t("selectAnM3u8File")
                          : tool === "format-convert"
                            ? t("selectAMediaFile")
                            : tool === "codec-convert"
                              ? t("selectAMediaFile")
                            : tool === "clip-video"
                              ? t("selectAVideoToClip")
                            : tool === "analyze-media"
                              ? t("selectAVideoFile")
                              : t("selectAMultiTrackHlsFolder"),
                  },
                ]}
              >
                <Input
                  readOnly
                  placeholder={
                    tool === "merge-ts"
                      ? t("selectAFolderContainingTsSegments")
                      : tool === "ts-to-mp4"
                        ? t("selectATsFileToConvert")
                        : tool === "local-m3u8-to-mp4"
                          ? t("selectAnM3u8FileToConvert")
                        : tool === "format-convert"
                          ? t("selectAMediaFileToConvert")
                          : tool === "codec-convert"
                            ? t("selectAMediaFileToTranscode")
                          : tool === "clip-video"
                            ? t("selectAnMp4M4vMovWebmFileToClip")
                          : tool === "analyze-media"
                            ? t("selectAVideoToAnalyze")
                            : t("selectAMultiTrackHlsFolderCreatedByThisApp")
                  }
                />
              </Form.Item>
              <Button
                icon={
                  tool === "ts-to-mp4" ||
                  tool === "local-m3u8-to-mp4" ||
                  tool === "format-convert" ||
                  tool === "codec-convert" ||
                  tool === "clip-video" ||
                  tool === "analyze-media" ? (
                    <FileOutlined />
                  ) : (
                    <FolderOpenOutlined />
                  )
                }
                onClick={() => void handlePickInput()}
              >
                {t("browse")}</Button>
            </Space.Compact>
          </Form.Item>
        )}

        {tool === "merge-video" && (
          <Form.Item
            label={t("mergeMode")}
            name="merge_mode"
            rules={[{ required: true, message: t("selectMergeMode") }]}
          >
            <Radio.Group optionType="button" buttonStyle="solid">
              {MERGE_VIDEO_MODE_OPTIONS().map((option) => (
                <Radio.Button key={option.value} value={option.value}>
                  {option.label}
                </Radio.Button>
              ))}
            </Radio.Group>
          </Form.Item>
        )}

        {tool === "clip-video" && (
          <>
            <Form.Item
              label={t("previewAndClipRange")}
              required
              name="clip_range"
              rules={[
                {
                  validator: async (_rule, value: ClipRange | undefined) => {
                    if (!value) {
                      throw new Error(t("selectAClipRangeInThePlayerFirst"));
                    }
                    if (!(value.end - value.start >= 0.05)) {
                      throw new Error(t("theClipMustBeAtLeast005SecondsLong"));
                    }
                  },
                },
              ]}
            >
              <VideoClipPicker
                inputPath={clipInputPath}
                onLoadStateChange={setClipStatus}
              />
            </Form.Item>
            <Form.Item
              label={t("clipMode")}
              name="clip_mode"
              rules={[{ required: true, message: t("selectClipMode") }]}
            >
              <Radio.Group optionType="button" buttonStyle="solid">
                {CLIP_MODE_OPTIONS().map((option) => (
                  <Radio.Button key={option.value} value={option.value}>
                    {option.label}
                  </Radio.Button>
                ))}
              </Radio.Group>
            </Form.Item>
          </>
        )}

        {tool === "format-convert" && (
          <Form.Item
            label={t("targetFormat")}
            name="target_format"
            rules={[{ required: true, message: t("selectTargetFormat") }]}
          >
            <Select
              options={CONVERT_FORMAT_OPTIONS}
              onChange={(value: ConvertFormat) => {
                const inputPath = form.getFieldValue("input_path") as string | undefined;
                if (!inputPath) {
                  return;
                }
                form.setFieldValue("output_path", buildConvertedOutputPath(inputPath, value));
              }}
            />
          </Form.Item>
        )}

        {tool === "codec-convert" && (
          <Form.Item
            label={t("outputFormat")}
            name="output_format"
            rules={[{ required: true, message: t("selectOutputFormat") }]}
          >
            <Select
              options={CODEC_OUTPUT_FORMAT_OPTIONS}
              onChange={(value: CodecOutputFormat) => {
                const inputPath = form.getFieldValue("input_path") as string | undefined;
                if (inputPath) {
                  form.setFieldValue("output_path", buildConvertedOutputPath(inputPath, value));
                }
                const nextVideoOptions = VIDEO_CODEC_OPTIONS_BY_FORMAT()[value];
                const nextAudioOptions = AUDIO_CODEC_OPTIONS_BY_FORMAT()[value];
                const currentVideoCodec = form.getFieldValue("video_codec") as VideoCodec | undefined;
                const currentAudioCodec = form.getFieldValue("audio_codec") as AudioCodec | undefined;
                if (!nextVideoOptions.some((option) => option.value === currentVideoCodec)) {
                  form.setFieldValue("video_codec", nextVideoOptions[0]?.value);
                }
                if (!nextAudioOptions.some((option) => option.value === currentAudioCodec)) {
                  form.setFieldValue("audio_codec", nextAudioOptions[0]?.value);
                }
              }}
            />
          </Form.Item>
        )}

        {tool === "format-convert" && (
          <Form.Item
            label={t("conversionMode")}
            name="convert_mode"
            rules={[{ required: true, message: t("selectConversionMode") }]}
          >
            <Radio.Group optionType="button" buttonStyle="solid">
              {CONVERT_MODE_OPTIONS().map((option) => (
                <Radio.Button key={option.value} value={option.value}>
                  {option.label}
                </Radio.Button>
              ))}
            </Radio.Group>
          </Form.Item>
        )}

        {tool === "codec-convert" && (
          <Form.Item
            label={t("videoCodec")}
            name="video_codec"
            rules={[{ required: true, message: t("selectVideoCodec") }]}
          >
            <Select
              options={
                VIDEO_CODEC_OPTIONS_BY_FORMAT()[
                  codecOutputFormat ?? "mp4"
                ]
              }
            />
          </Form.Item>
        )}

        {tool === "codec-convert" && (
          <Form.Item
            label={t("audioCodec")}
            name="audio_codec"
            rules={[{ required: true, message: t("selectAudioCodec") }]}
          >
            <Select
              options={
                AUDIO_CODEC_OPTIONS_BY_FORMAT()[
                  codecOutputFormat ?? "mp4"
                ]
              }
            />
          </Form.Item>
        )}

        {tool !== "analyze-media" && (
          <Form.Item label={t("outputFile")} required>
            <Space.Compact style={{ width: "100%" }}>
              <Form.Item
                name="output_path"
                noStyle
                rules={[{ required: true, message: t("selectAnOutputFile") }]}
              >
                <Input readOnly placeholder={t("selectAnOutputFile")} />
              </Form.Item>
              <Button icon={<FolderOpenOutlined />} onClick={() => void handlePickOutput()}>
                {t("browse")}</Button>
            </Space.Compact>
          </Form.Item>
        )}

        {tool === "ts-to-mp4" && (
          <Typography.Text type="secondary">
            {t("thisToolCreatesAnMp4FileAndKeepsTheOriginal")}</Typography.Text>
        )}
        {tool === "local-m3u8-to-mp4" && (
          <Typography.Text type="secondary">
            {t("allProcessingIsLocalReadsTheSelectedM3u8SegmentsAnd")}</Typography.Text>
        )}
        {tool === "merge-video" && (
          <Typography.Text type="secondary">
            {t("fastMergeJoinsStreamsDirectlyWhenPossibleAndRequiresMatching")}</Typography.Text>
        )}
        {tool === "clip-video" && (
          <Typography.Text type="secondary">
            {t("fastModeCopiesStreamsWithoutReEncodingAndCutsNear")}</Typography.Text>
        )}
        {tool === "format-convert" && (
          <Typography.Text type="secondary">
            {t("fastConversionIsTheDefaultAndCopiesStreamsWithoutRe")}</Typography.Text>
        )}
        {tool === "codec-convert" && (
          <Typography.Text type="secondary">
            {t("thisToolReEncodesVideoAndAudioTracksMp4H")}</Typography.Text>
        )}
        {tool === "analyze-media" && (
          <Typography.Text type="secondary">
            {t("readsTheContainerDurationBitrateAndMediaTracksAndDisplays")}</Typography.Text>
        )}
        {tool === "multi-track-hls-to-mp4" && (
          <Typography.Text type="secondary">
            {t("onlyMultiTrackHlsFoldersCreatedByThisAppAre")}</Typography.Text>
        )}

        {tool === "analyze-media" && analysisResult && (
          <>
            <Divider style={{ margin: "16px 0" }}>{t("analysisResults")}</Divider>
            <Descriptions
              size="small"
              bordered
              column={2}
              items={[
                { key: "path", label: t("filePath"), children: analysisResult.file_path, span: 2 },
                {
                  key: "format",
                  label: t("containerFormat"),
                  children: analysisResult.format_long_name || analysisResult.format_name || "-",
                },
                {
                  key: "streams",
                  label: t("streamCount"),
                  children: String(analysisResult.stream_count),
                },
                {
                  key: "duration",
                  label: t("duration"),
                  children: formatDuration(analysisResult.duration),
                },
                {
                  key: "size",
                  label: t("fileSize"),
                  children: formatBytes(analysisResult.size),
                },
                {
                  key: "bitrate",
                  label: t("totalBitrate"),
                  children: formatBitRate(analysisResult.bit_rate),
                },
                {
                  key: "probe-score",
                  label: t("probeScore"),
                  children:
                    analysisResult.probe_score === null ? "-" : String(analysisResult.probe_score),
                },
              ]}
            />

            {renderStreamSection(t("videoTracks"), analysisResult.video_streams)}
            {renderStreamSection(t("audioTracks"), analysisResult.audio_streams)}
            {renderStreamSection(t("subtitleTracks"), analysisResult.subtitle_streams)}
            {renderStreamSection(t("otherTracks"), analysisResult.other_streams)}

            <Divider style={{ margin: "16px 0 8px" }}>{t("fullRawOutput")}</Divider>
            <Input.TextArea
              readOnly
              value={analysisResult.raw_json}
              autoSize={{ minRows: 12, maxRows: 20 }}
            />
          </>
        )}
      </Form>
    </Modal>
  );
}

function renderStreamSection(title: string, streams: MediaAnalysisResult["video_streams"]) {
  if (!streams.length) {
    return null;
  }

  return (
    <>
      <Divider style={{ margin: "16px 0 8px" }}>{title}</Divider>
      <Space direction="vertical" size={8} style={{ width: "100%" }}>
        {streams.map((stream) => (
          <Descriptions
            key={`${title}-${stream.index}`}
            size="small"
            bordered
            column={2}
            items={[
              {
                key: "index",
                label: t("track"),
                children: `#${stream.index}`,
              },
              {
                key: "codec",
                label: t("codec2"),
                children: stream.codec_long_name || stream.codec_name || "-",
              },
              {
                key: "profile",
                label: "Profile",
                children: stream.profile || "-",
              },
              {
                key: "language",
                label: t("language"),
                children: stream.language || "-",
              },
              {
                key: "resolution",
                label: t("resolution2"),
                children:
                  stream.width && stream.height ? `${stream.width} x ${stream.height}` : "-",
              },
              {
                key: "pixel",
                label: t("pixelFormat"),
                children: stream.pix_fmt || "-",
              },
              {
                key: "fps",
                label: t("frameRate2"),
                children: stream.avg_frame_rate || stream.r_frame_rate || "-",
              },
              {
                key: "sample-rate",
                label: t("sampleRate"),
                children: stream.sample_rate || "-",
              },
              {
                key: "channels",
                label: t("channels"),
                children:
                  stream.channels === null
                    ? "-"
                    : stream.channel_layout
                      ? `${stream.channels} (${stream.channel_layout})`
                      : String(stream.channels),
              },
              {
                key: "bitrate",
                label: t("bitrate"),
                children: formatBitRate(stream.bit_rate),
              },
              {
                key: "duration",
                label: t("duration"),
                children: formatDuration(stream.duration),
              },
              {
                key: "level",
                label: "Level",
                children: stream.level === null ? "-" : String(stream.level),
              },
            ]}
          />
        ))}
      </Space>
    </>
  );
}

function splitPath(path: string) {
  const normalized = path.replace(/\\/g, "/");
  const lastSlashIndex = normalized.lastIndexOf("/");
  const dir = lastSlashIndex >= 0 ? normalized.slice(0, lastSlashIndex) : "";
  const name = lastSlashIndex >= 0 ? normalized.slice(lastSlashIndex + 1) : normalized;
  return { dir, name };
}

function joinPath(dir: string, name: string) {
  if (!dir) return name;
  return `${dir}/${name}`;
}

function getPathName(path: string) {
  return splitPath(path).name || path;
}

function buildMergedOutputPath(inputDir: string) {
  const { dir, name } = splitPath(inputDir);
  const normalizedName = (name || "merged")
    .replace(/^\.+/, "")
    .replace(/^m3u8quicker_temp_/, "")
    .trim();
  return joinPath(dir, `${normalizedName || "merged"}.ts`);
}

function buildMp4OutputPath(inputPath: string) {
  const { dir, name } = splitPath(inputPath);
  const nextName = name.toLowerCase().endsWith(".ts")
    ? `${name.slice(0, -3)}.mp4`
    : `${name}.mp4`;
  return joinPath(dir, nextName);
}

function buildLocalM3u8Mp4OutputPath(inputPath: string) {
  const { dir, name } = splitPath(inputPath);
  const nextName = name.toLowerCase().endsWith(".m3u8")
    ? `${name.slice(0, -5)}.mp4`
    : `${name}.mp4`;
  return joinPath(dir, nextName);
}

function buildMergedVideoOutputPath(inputPaths: string[]) {
  const firstInput = inputPaths[0];
  if (!firstInput) {
    return "merged.mp4";
  }

  const { dir, name } = splitPath(firstInput);
  const dotIndex = name.lastIndexOf(".");
  const baseName = dotIndex > 0 ? name.slice(0, dotIndex) : name;
  return joinPath(dir, `${baseName || "merged"}-merged.mp4`);
}

function buildConvertedOutputPath(inputPath: string, targetFormat: ConvertFormat) {
  const { dir, name } = splitPath(inputPath);
  const dotIndex = name.lastIndexOf(".");
  const baseName = dotIndex > 0 ? name.slice(0, dotIndex) : name;
  return joinPath(dir, `${baseName || "output"}.${targetFormat}`);
}

function buildClipVideoOutputPath(inputPath: string) {
  const { dir, name } = splitPath(inputPath);
  const dotIndex = name.lastIndexOf(".");
  const baseName = dotIndex > 0 ? name.slice(0, dotIndex) : name;
  return joinPath(dir, `${baseName || "output"}_clip.mp4`);
}

function buildMultiTrackMp4OutputPath(inputDir: string) {
  const { dir, name } = splitPath(inputDir);
  const sanitizedName = (name || "bundle").replace(/^\.+/, "").trim();
  const strippedName = sanitizedName.replace(/_tracks$/i, "").trim();
  const outputName = strippedName || sanitizedName || "bundle";
  return joinPath(dir, `${outputName}.mp4`);
}

function formatToolError(error: unknown) {
  const text = String(error ?? "").trim();
  if (!text) {
    return t("unknownError");
  }

  return text.replace(
    /^(Invalid input|M3U8 parse error|Network error|IO error|URL parse error|Decryption error|Conversion error):\s*/i,
    ""
  );
}

function formatDuration(value: string | null | undefined) {
  if (!value) {
    return "-";
  }

  const seconds = Number(value);
  if (!Number.isFinite(seconds)) {
    return value;
  }

  if (seconds < 60) {
    return t("sec2", { value0: seconds.toFixed(2) });
  }

  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const restSeconds = seconds % 60;
  if (hours > 0) {
    return t("hrMinSec", { value0: hours, value1: minutes, value2: restSeconds.toFixed(2) });
  }
  return t("minSec", { value0: minutes, value1: restSeconds.toFixed(2) });
}

function formatBytes(value: string | null | undefined) {
  if (!value) {
    return "-";
  }

  const bytes = Number(value);
  if (!Number.isFinite(bytes)) {
    return value;
  }

  if (bytes < 1024) {
    return `${bytes.toFixed(0)} B`;
  }

  const units = ["KB", "MB", "GB", "TB"];
  let nextValue = bytes / 1024;
  let unitIndex = 0;

  while (nextValue >= 1024 && unitIndex < units.length - 1) {
    nextValue /= 1024;
    unitIndex += 1;
  }

  return `${nextValue.toFixed(2)} ${units[unitIndex]}`;
}

function formatBitRate(value: string | null | undefined) {
  if (!value) {
    return "-";
  }

  const bitRate = Number(value);
  if (!Number.isFinite(bitRate)) {
    return value;
  }

  if (bitRate < 1000) {
    return `${bitRate.toFixed(0)} bps`;
  }
  if (bitRate < 1_000_000) {
    return `${(bitRate / 1000).toFixed(2)} Kbps`;
  }
  return `${(bitRate / 1_000_000).toFixed(2)} Mbps`;
}
