import { useEffect, useMemo, useState } from "react";
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
  SwapOutlined,
} from "@ant-design/icons";
import { open as pickDialogPath, save } from "@tauri-apps/plugin-dialog";
import {
  analyzeMediaFile,
  convertLocalM3u8ToMp4File,
  convertMultiTrackHlsToMp4Dir,
  convertMediaFile,
  convertTsToMp4File,
  mergeVideoFiles,
  mergeTsFiles,
  transcodeMediaFile,
} from "../services/api";
import type { MediaAnalysisResult } from "../types";

export type ToolAction =
  | "merge-ts"
  | "ts-to-mp4"
  | "local-m3u8-to-mp4"
  | "merge-video"
  | "format-convert"
  | "codec-convert"
  | "analyze-media"
  | "multi-track-hls-to-mp4"
  | "install-chrome-extension"
  | "install-edge-extension"
  | "install-firefox-extension";

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

const CONVERT_MODE_OPTIONS: Array<{ value: ConvertMode; label: string }> = [
  { value: "quick", label: "快速转换" },
  { value: "compatible", label: "兼容转换" },
];

const MERGE_VIDEO_MODE_OPTIONS: Array<{ value: MergeVideoMode; label: string }> = [
  { value: "fast", label: "极速合并" },
  { value: "compatible", label: "兼容合并" },
];

const CODEC_OUTPUT_FORMAT_OPTIONS: Array<{ value: CodecOutputFormat; label: string }> = [
  { value: "mp4", label: "MP4" },
  { value: "mkv", label: "MKV" },
  { value: "mov", label: "MOV" },
];

const VIDEO_CODEC_OPTIONS_BY_FORMAT: Record<
  CodecOutputFormat,
  Array<{ value: VideoCodec; label: string }>
> = {
  mp4: [
    { value: "h264", label: "H.264" },
    { value: "h265", label: "H.265" },
    { value: "copy", label: "复制原视频编码" },
  ],
  mkv: [
    { value: "h264", label: "H.264" },
    { value: "h265", label: "H.265" },
    { value: "vp9", label: "VP9" },
    { value: "copy", label: "复制原视频编码" },
  ],
  mov: [
    { value: "h264", label: "H.264" },
    { value: "h265", label: "H.265" },
    { value: "copy", label: "复制原视频编码" },
  ],
};

const AUDIO_CODEC_OPTIONS_BY_FORMAT: Record<
  CodecOutputFormat,
  Array<{ value: AudioCodec; label: string }>
> = {
  mp4: [
    { value: "aac", label: "AAC" },
    { value: "mp3", label: "MP3" },
    { value: "copy", label: "复制原音频编码" },
  ],
  mkv: [
    { value: "aac", label: "AAC" },
    { value: "mp3", label: "MP3" },
    { value: "opus", label: "Opus" },
    { value: "copy", label: "复制原音频编码" },
  ],
  mov: [
    { value: "aac", label: "AAC" },
    { value: "copy", label: "复制原音频编码" },
  ],
};

interface ToolsModalProps {
  open: boolean;
  tool: ToolAction | null;
  onClose: () => void;
}

export function ToolsModal({ open, tool, onClose }: ToolsModalProps) {
  const [form] = Form.useForm();
  const [submitting, setSubmitting] = useState(false);
  const [analysisResult, setAnalysisResult] = useState<MediaAnalysisResult | null>(null);
  const codecOutputFormat = Form.useWatch("output_format", form) as
    | CodecOutputFormat
    | undefined;
  const mergeVideoInputPaths = Form.useWatch("input_paths", form) as string[] | undefined;

  const title = useMemo(() => {
    if (tool === "merge-ts") {
      return (
        <Space size={8}>
          <MergeCellsOutlined />
          <span>合并 ts</span>
        </Space>
      );
    }

    if (tool === "ts-to-mp4") {
      return (
        <Space size={8}>
          <SwapOutlined />
          <span>ts 转 mp4</span>
        </Space>
      );
    }

    if (tool === "local-m3u8-to-mp4") {
      return (
        <Space size={8}>
          <FileSyncOutlined />
          <span>本地 m3u8 转 mp4</span>
        </Space>
      );
    }

    if (tool === "merge-video") {
      return (
        <Space size={8}>
          <MergeCellsOutlined />
          <span>合并视频</span>
        </Space>
      );
    }

    if (tool === "format-convert") {
      return (
        <Space size={8}>
          <SwapOutlined />
          <span>格式转换</span>
        </Space>
      );
    }

    if (tool === "codec-convert") {
      return (
        <Space size={8}>
          <RetweetOutlined />
          <span>编码转换</span>
        </Space>
      );
    }

    if (tool === "analyze-media") {
      return (
        <Space size={8}>
          <FileSearchOutlined />
          <span>分析视频</span>
        </Space>
      );
    }

    if (tool === "multi-track-hls-to-mp4") {
      return (
        <Space size={8}>
          <ApartmentOutlined />
          <span>多轨 HLS 转 mp4</span>
        </Space>
      );
    }

    return "工具";
  }, [tool]);

  useEffect(() => {
    if (!open) return;
    form.resetFields();
    setAnalysisResult(null);
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
  }, [form, open, tool]);

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
            name: "视频文件",
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
      tool === "codec-convert"
    ) {
      const selected = await pickDialogPath({
        multiple: false,
        directory: false,
        filters:
          tool === "ts-to-mp4"
            ? [{ name: "TS 文件", extensions: ["ts"] }]
            : tool === "local-m3u8-to-mp4"
              ? [{ name: "M3U8 文件", extensions: ["m3u8"] }]
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
          ? [{ name: "TS 文件", extensions: ["ts"] }]
          : tool === "merge-video"
            ? [{ name: "MP4 文件", extensions: ["mp4"] }]
          : tool === "codec-convert"
            ? [{ name: `${outputFormat.toUpperCase()} 文件`, extensions: [outputFormat] }]
          : tool === "format-convert"
            ? [{ name: `${targetFormat.toUpperCase()} 文件`, extensions: [targetFormat] }]
            : [{ name: "MP4 文件", extensions: ["mp4"] }],
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
            ? "ts 已合并完成"
            : `ts 已合并完成，已另存为 ${getPathName(savedPath)}`
        );
      } else if (tool === "ts-to-mp4") {
        const requestedOutput = values.output_path.trim();
        const savedPath = await convertTsToMp4File(values.input_path.trim(), requestedOutput);
        message.success(
          savedPath === requestedOutput
            ? "mp4 已生成，原 ts 文件已保留"
            : `mp4 已生成，原 ts 文件已保留，已另存为 ${getPathName(savedPath)}`
        );
      } else if (tool === "local-m3u8-to-mp4") {
        const requestedOutput = values.output_path.trim();
        const savedPath = await convertLocalM3u8ToMp4File(
          values.input_path.trim(),
          requestedOutput
        );
        message.success(
          savedPath === requestedOutput
            ? "m3u8 已转换为 mp4，原文件已保留"
            : `m3u8 已转换为 mp4，原文件已保留，已另存为 ${getPathName(savedPath)}`
        );
      } else if (tool === "merge-video") {
        const inputPaths =
          (form.getFieldValue("input_paths") as string[] | undefined) ?? [];
        if (inputPaths.length < 2) {
          message.error("请至少选择两个视频文件");
          return;
        }
        const requestedOutput = values.output_path.trim();
        const mergeMode = (values.merge_mode as MergeVideoMode | undefined) ?? "fast";
        const savedPath = await mergeVideoFiles(inputPaths, requestedOutput, mergeMode);
        message.success(
          savedPath === requestedOutput
            ? "视频已合并完成，原文件已保留"
            : `视频已合并完成，原文件已保留，已另存为 ${getPathName(savedPath)}`
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
            ? `${formatLabel} 已生成，原文件已保留`
            : `${formatLabel} 已生成，原文件已保留，已另存为 ${getPathName(savedPath)}`
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
            ? `${formatLabel} 编码转换完成，原文件已保留`
            : `${formatLabel} 编码转换完成，原文件已保留，已另存为 ${getPathName(savedPath)}`
        );
      } else if (tool === "analyze-media") {
        const result = await analyzeMediaFile(values.input_path.trim());
        setAnalysisResult(result);
        message.success("视频信息已分析完成");
      } else {
        const requestedOutput = values.output_path.trim();
        const savedPath = await convertMultiTrackHlsToMp4Dir(
          values.input_path.trim(),
          requestedOutput
        );
        message.success(
          savedPath === requestedOutput
            ? "多轨 HLS 已转为 mp4，原目录已保留"
            : `多轨 HLS 已转为 mp4，原目录已保留，已另存为 ${getPathName(savedPath)}`
        );
      }

      if (tool !== "analyze-media") {
        onClose();
      }
    } catch (error: unknown) {
      if (error && typeof error === "object" && "errorFields" in error) return;
      message.error(`执行工具失败: ${formatToolError(error)}`);
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
      okText={tool === "analyze-media" ? "开始分析" : "开始处理"}
      cancelText="取消"
      confirmLoading={submitting}
      destroyOnClose
      width={tool === "analyze-media" ? 760 : tool === "merge-video" ? 720 : 520}
    >
      <Form form={form} layout="vertical">
        {tool === "merge-video" ? (
          <Form.Item label="视频文件" required>
            <Space direction="vertical" size={8} style={{ width: "100%" }}>
              <Button icon={<FileOutlined />} onClick={() => void handlePickInput()}>
                选择多个视频
              </Button>
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
                  placeholder="请选择至少两个待拼接的视频文件"
                />
              )}
            </Space>
          </Form.Item>
        ) : (
          <Form.Item
            label={
              tool === "merge-ts"
                ? "TS 目录"
                : tool === "ts-to-mp4"
                  ? "TS 文件"
                  : tool === "local-m3u8-to-mp4"
                    ? "M3U8 文件"
                  : tool === "format-convert"
                    ? "媒体文件"
                    : tool === "codec-convert"
                      ? "媒体文件"
                    : tool === "analyze-media"
                      ? "视频文件"
                      : "多轨 HLS 目录"
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
                        ? "请选择 TS 目录"
                        : tool === "ts-to-mp4"
                          ? "请选择 TS 文件"
                          : tool === "local-m3u8-to-mp4"
                            ? "请选择 m3u8 文件"
                          : tool === "format-convert"
                            ? "请选择媒体文件"
                            : tool === "codec-convert"
                              ? "请选择媒体文件"
                            : tool === "analyze-media"
                              ? "请选择视频文件"
                              : "请选择多轨 HLS 目录",
                  },
                ]}
              >
                <Input
                  readOnly
                  placeholder={
                    tool === "merge-ts"
                      ? "请选择包含 ts 切片的目录"
                      : tool === "ts-to-mp4"
                        ? "请选择待转换的 ts 文件"
                        : tool === "local-m3u8-to-mp4"
                          ? "请选择待转换的 m3u8 文件"
                        : tool === "format-convert"
                          ? "请选择待转换的媒体文件"
                          : tool === "codec-convert"
                            ? "请选择待进行编码转换的媒体文件"
                          : tool === "analyze-media"
                            ? "请选择待分析的视频文件"
                            : "请选择本应用生成的多轨 HLS 目录"
                  }
                />
              </Form.Item>
              <Button
                icon={
                  tool === "ts-to-mp4" ||
                  tool === "local-m3u8-to-mp4" ||
                  tool === "format-convert" ||
                  tool === "codec-convert" ||
                  tool === "analyze-media" ? (
                    <FileOutlined />
                  ) : (
                    <FolderOpenOutlined />
                  )
                }
                onClick={() => void handlePickInput()}
              >
                选择
              </Button>
            </Space.Compact>
          </Form.Item>
        )}

        {tool === "merge-video" && (
          <Form.Item
            label="合并模式"
            name="merge_mode"
            rules={[{ required: true, message: "请选择合并模式" }]}
          >
            <Radio.Group optionType="button" buttonStyle="solid">
              {MERGE_VIDEO_MODE_OPTIONS.map((option) => (
                <Radio.Button key={option.value} value={option.value}>
                  {option.label}
                </Radio.Button>
              ))}
            </Radio.Group>
          </Form.Item>
        )}

        {tool === "format-convert" && (
          <Form.Item
            label="目标格式"
            name="target_format"
            rules={[{ required: true, message: "请选择目标格式" }]}
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
            label="输出格式"
            name="output_format"
            rules={[{ required: true, message: "请选择输出格式" }]}
          >
            <Select
              options={CODEC_OUTPUT_FORMAT_OPTIONS}
              onChange={(value: CodecOutputFormat) => {
                const inputPath = form.getFieldValue("input_path") as string | undefined;
                if (inputPath) {
                  form.setFieldValue("output_path", buildConvertedOutputPath(inputPath, value));
                }
                const nextVideoOptions = VIDEO_CODEC_OPTIONS_BY_FORMAT[value];
                const nextAudioOptions = AUDIO_CODEC_OPTIONS_BY_FORMAT[value];
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
            label="转换模式"
            name="convert_mode"
            rules={[{ required: true, message: "请选择转换模式" }]}
          >
            <Radio.Group optionType="button" buttonStyle="solid">
              {CONVERT_MODE_OPTIONS.map((option) => (
                <Radio.Button key={option.value} value={option.value}>
                  {option.label}
                </Radio.Button>
              ))}
            </Radio.Group>
          </Form.Item>
        )}

        {tool === "codec-convert" && (
          <Form.Item
            label="视频编码"
            name="video_codec"
            rules={[{ required: true, message: "请选择视频编码" }]}
          >
            <Select
              options={
                VIDEO_CODEC_OPTIONS_BY_FORMAT[
                  codecOutputFormat ?? "mp4"
                ]
              }
            />
          </Form.Item>
        )}

        {tool === "codec-convert" && (
          <Form.Item
            label="音频编码"
            name="audio_codec"
            rules={[{ required: true, message: "请选择音频编码" }]}
          >
            <Select
              options={
                AUDIO_CODEC_OPTIONS_BY_FORMAT[
                  codecOutputFormat ?? "mp4"
                ]
              }
            />
          </Form.Item>
        )}

        {tool !== "analyze-media" && (
          <Form.Item label="输出文件" required>
            <Space.Compact style={{ width: "100%" }}>
              <Form.Item
                name="output_path"
                noStyle
                rules={[{ required: true, message: "请选择输出文件" }]}
              >
                <Input readOnly placeholder="请选择输出文件" />
              </Form.Item>
              <Button icon={<FolderOpenOutlined />} onClick={() => void handlePickOutput()}>
                选择
              </Button>
            </Space.Compact>
          </Form.Item>
        )}

        {tool === "ts-to-mp4" && (
          <Typography.Text type="secondary">
            该工具会保留原 ts 文件，只额外生成一个 mp4 文件。
          </Typography.Text>
        )}
        {tool === "local-m3u8-to-mp4" && (
          <Typography.Text type="secondary">
            该工具完全在本地处理：解析所选 m3u8、读取同目录下的分片与密钥文件，自动解密 AES-128 后生成 mp4，全程不会发起任何网络请求。
          </Typography.Text>
        )}
        {tool === "merge-video" && (
          <Typography.Text type="secondary">
            极速合并会尽量直接拼接，速度更快，但要求分辨率、编码和音频轨规格一致；兼容合并会统一规格后再拼接，适合不同分辨率的视频。
          </Typography.Text>
        )}
        {tool === "format-convert" && (
          <Typography.Text type="secondary">
            默认使用快速转换，不重编码，速度更快；兼容转换会重新编码，适合目标格式兼容性要求更高的场景。
          </Typography.Text>
        )}
        {tool === "codec-convert" && (
          <Typography.Text type="secondary">
            该工具用于重新编码视频和音频轨道。建议优先选择 MP4 + H.264 + AAC，兼容性最好。
          </Typography.Text>
        )}
        {tool === "analyze-media" && (
          <Typography.Text type="secondary">
            会读取视频封装、时长、码率以及各个音视频轨道信息，并展示完整 ffprobe 原始结果。
          </Typography.Text>
        )}
        {tool === "multi-track-hls-to-mp4" && (
          <Typography.Text type="secondary">
            仅支持本应用生成的多轨 HLS 目录，会按设置中的 FFmpeg 路径处理并保留原目录。
          </Typography.Text>
        )}

        {tool === "analyze-media" && analysisResult && (
          <>
            <Divider style={{ margin: "16px 0" }}>分析结果</Divider>
            <Descriptions
              size="small"
              bordered
              column={2}
              items={[
                { key: "path", label: "文件路径", children: analysisResult.file_path, span: 2 },
                {
                  key: "format",
                  label: "封装格式",
                  children: analysisResult.format_long_name || analysisResult.format_name || "-",
                },
                {
                  key: "streams",
                  label: "流数量",
                  children: String(analysisResult.stream_count),
                },
                {
                  key: "duration",
                  label: "时长",
                  children: formatDuration(analysisResult.duration),
                },
                {
                  key: "size",
                  label: "文件大小",
                  children: formatBytes(analysisResult.size),
                },
                {
                  key: "bitrate",
                  label: "总码率",
                  children: formatBitRate(analysisResult.bit_rate),
                },
                {
                  key: "probe-score",
                  label: "探测分数",
                  children:
                    analysisResult.probe_score === null ? "-" : String(analysisResult.probe_score),
                },
              ]}
            />

            {renderStreamSection("视频轨", analysisResult.video_streams)}
            {renderStreamSection("音频轨", analysisResult.audio_streams)}
            {renderStreamSection("字幕轨", analysisResult.subtitle_streams)}
            {renderStreamSection("其他轨", analysisResult.other_streams)}

            <Divider style={{ margin: "16px 0 8px" }}>完整原始信息</Divider>
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
                label: "轨道",
                children: `#${stream.index}`,
              },
              {
                key: "codec",
                label: "编码",
                children: stream.codec_long_name || stream.codec_name || "-",
              },
              {
                key: "profile",
                label: "Profile",
                children: stream.profile || "-",
              },
              {
                key: "language",
                label: "语言",
                children: stream.language || "-",
              },
              {
                key: "resolution",
                label: "分辨率",
                children:
                  stream.width && stream.height ? `${stream.width} x ${stream.height}` : "-",
              },
              {
                key: "pixel",
                label: "像素格式",
                children: stream.pix_fmt || "-",
              },
              {
                key: "fps",
                label: "帧率",
                children: stream.avg_frame_rate || stream.r_frame_rate || "-",
              },
              {
                key: "sample-rate",
                label: "采样率",
                children: stream.sample_rate || "-",
              },
              {
                key: "channels",
                label: "声道",
                children:
                  stream.channels === null
                    ? "-"
                    : stream.channel_layout
                      ? `${stream.channels} (${stream.channel_layout})`
                      : String(stream.channels),
              },
              {
                key: "bitrate",
                label: "码率",
                children: formatBitRate(stream.bit_rate),
              },
              {
                key: "duration",
                label: "时长",
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
    return "未知错误";
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
    return `${seconds.toFixed(2)} 秒`;
  }

  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const restSeconds = seconds % 60;
  if (hours > 0) {
    return `${hours} 小时 ${minutes} 分 ${restSeconds.toFixed(2)} 秒`;
  }
  return `${minutes} 分 ${restSeconds.toFixed(2)} 秒`;
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
