import {
  cloneElement,
  useEffect,
  useMemo,
  useState,
  type CSSProperties,
  type ReactElement,
  type ReactNode,
} from "react";
import { Alert, Empty, Image, Select, Space, Spin, Tag, Tooltip, Typography, message, theme } from "antd";
import type { GlobalToken } from "antd";
import { save } from "@tauri-apps/plugin-dialog";
import {
  AppstoreOutlined,
  DownloadOutlined,
  MinusOutlined,
  PictureOutlined,
  PlusOutlined,
} from "@ant-design/icons";
import { convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  extractPreviewThumbnails,
  getAppSettings,
  setPreviewColumns,
  setPreviewThumbnailSettings,
  type PreviewThumbnail,
} from "../services/api";

const MIN_COUNT = 9;
const MAX_COUNT = 99;
const STEP = 9;
const DEFAULT_COLUMNS = 3;
const MIN_COLUMNS = 1;
const MAX_COLUMNS = 12;
const DEFAULT_THUMBNAIL_WIDTH = 320;
const DEFAULT_JPEG_QUALITY = 4;

const WIDTH_OPTIONS = [
  { value: 320, label: "320 px" },
  { value: 640, label: "640 px" },
  { value: 960, label: "960 px" },
  { value: 1280, label: "1280 px" },
  { value: 1920, label: "1920 px" },
];

const QUALITY_OPTIONS = [
  { value: 2, label: "高" },
  { value: 4, label: "标准" },
  { value: 6, label: "较小" },
  { value: 8, label: "小" },
  { value: 10, label: "最小" },
];

type DirectoryPickerWindow = Window &
  typeof globalThis & {
    showDirectoryPicker?: () => Promise<FileSystemDirectoryHandle>;
  };

const STEPPER_HEIGHT = 30;

function buildStepperWrapperStyle(token: GlobalToken): CSSProperties {
  return {
    display: "inline-flex",
    alignItems: "stretch",
    height: STEPPER_HEIGHT,
    borderRadius: 8,
    overflow: "hidden",
    border: `1px solid ${token.colorBorderSecondary}`,
    background: token.colorBgContainer,
    boxShadow: token.boxShadowTertiary,
  };
}

function buildStepperButtonStyle(token: GlobalToken): CSSProperties {
  return {
    width: 30,
    height: "100%",
    padding: 0,
    border: 0,
    background: "transparent",
    color: token.colorTextSecondary,
    cursor: "pointer",
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    fontSize: 12,
    transition: "background 0.15s ease, color 0.15s ease",
  };
}

function buildStepperButtonDisabledStyle(token: GlobalToken): CSSProperties {
  return {
    cursor: "not-allowed",
    color: token.colorTextDisabled,
    background: "transparent",
  };
}

function buildStepperLabelStyle(token: GlobalToken): CSSProperties {
  return {
    minWidth: 80,
    padding: "0 8px",
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    gap: 5,
    borderLeft: `1px solid ${token.colorBorderSecondary}`,
    borderRight: `1px solid ${token.colorBorderSecondary}`,
    background: token.colorFillQuaternary,
    fontSize: 12,
    whiteSpace: "nowrap",
    color: token.colorText,
  };
}

interface PreviewThumbnailEvent {
  token: string;
  count: number;
  target_width: number;
  jpeg_quality: number;
  thumbnail: PreviewThumbnail;
}

export function PreviewWindow() {
  const token = useMemo(
    () => new URLSearchParams(window.location.search).get("token") ?? "",
    []
  );
  const [count, setCount] = useState(MIN_COUNT);
  const [columns, setColumns] = useState(DEFAULT_COLUMNS);
  const [thumbnailWidth, setThumbnailWidth] = useState(DEFAULT_THUMBNAIL_WIDTH);
  const [jpegQuality, setJpegQuality] = useState(DEFAULT_JPEG_QUALITY);
  const [thumbnails, setThumbnails] = useState<PreviewThumbnail[]>([]);
  const [loadedKey, setLoadedKey] = useState<number | null>(null);
  const [errorText, setErrorText] = useState<string | null>(
    token ? null : "预览参数缺失，无法打开窗口。"
  );
  const countOptions = useMemo(
    () =>
      Array.from(
        { length: Math.floor((MAX_COUNT - MIN_COUNT) / STEP) + 1 },
        (_, index) => {
          const value = MIN_COUNT + index * STEP;
          return { value, label: `${value} 张` };
        }
      ),
    []
  );
  const loading = Boolean(token) && loadedKey !== count;

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    void listen<PreviewThumbnailEvent>("preview-thumbnail", (event) => {
      const payload = event.payload;
      if (
        payload.token !== token ||
        payload.count !== count ||
        payload.target_width !== thumbnailWidth ||
        payload.jpeg_quality !== jpegQuality
      ) {
        return;
      }
      setThumbnails((current) =>
        upsertThumbnail(current, payload.thumbnail)
      );
    }).then((fn) => {
      if (cancelled) {
        fn();
        return [];
      }
      unlisten = fn;
      return extractPreviewThumbnails(token, count, thumbnailWidth, jpegQuality);
    }).then((items) => {
      if (cancelled) return;
      setThumbnails(sortThumbnails(items));
      setErrorText(null);
      setLoadedKey(count);
    }).catch((error) => {
      if (cancelled) return;
      setErrorText(formatError(error));
      setLoadedKey(count);
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [token, count, thumbnailWidth, jpegQuality]);

  useEffect(() => {
    let disposed = false;
    getAppSettings()
      .then((settings) => {
        if (disposed) return;
        setColumns(clampColumns(settings.preview_columns));
        setThumbnailWidth(clampThumbnailWidth(settings.preview_thumbnail_width));
        setJpegQuality(clampJpegQuality(settings.preview_jpeg_quality));
      })
      .catch((error) => {
        console.debug("Failed to load preview columns setting", error);
      });

    return () => {
      disposed = true;
    };
  }, []);

  const resetPreviewState = () => {
    setThumbnails([]);
    setLoadedKey(null);
    setErrorText(null);
  };

  const handleDecrement = () => {
    resetPreviewState();
    setCount((current) => Math.max(MIN_COUNT, current - STEP));
  };
  const handleIncrement = () => {
    resetPreviewState();
    setCount((current) => Math.min(MAX_COUNT, current + STEP));
  };
  const handleCountChange = (nextCount: number) => {
    if (nextCount === count) return;
    resetPreviewState();
    setCount(clampCount(nextCount));
  };
  const handleThumbnailWidthChange = (nextWidth: number) => {
    const normalizedWidth = clampThumbnailWidth(nextWidth);
    if (normalizedWidth === thumbnailWidth) return;
    resetPreviewState();
    setThumbnailWidth(normalizedWidth);
    void setPreviewThumbnailSettings(normalizedWidth, jpegQuality).catch((error) => {
      console.debug("Failed to save preview thumbnail settings", error);
    });
  };
  const handleJpegQualityChange = (nextQuality: number) => {
    const normalizedQuality = clampJpegQuality(nextQuality);
    if (normalizedQuality === jpegQuality) return;
    resetPreviewState();
    setJpegQuality(normalizedQuality);
    void setPreviewThumbnailSettings(thumbnailWidth, normalizedQuality).catch((error) => {
      console.debug("Failed to save preview thumbnail settings", error);
    });
  };
  const handleColumnsDecrement = () => {
    updateColumns(columns - 1);
  };
  const handleColumnsIncrement = () => {
    updateColumns(columns + 1);
  };
  const updateColumns = (nextColumns: number) => {
    const normalizedColumns = clampColumns(nextColumns);
    if (normalizedColumns === columns) return;
    setColumns(normalizedColumns);
    void setPreviewColumns(normalizedColumns).catch((error) => {
      console.debug("Failed to save preview columns setting", error);
    });
  };
  const { token: themeToken } = theme.useToken();
  const iconStyle: CSSProperties = { color: themeToken.colorPrimary };
  return (
    <div
      style={{
        height: "100vh",
        display: "flex",
        flexDirection: "column",
        background: themeToken.colorBgLayout,
        color: themeToken.colorText,
      }}
    >
      <div
        style={{
          padding: "12px 16px",
          borderBottom: `1px solid ${themeToken.colorBorderSecondary}`,
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 12,
          flexWrap: "wrap",
          background: themeToken.colorBgContainer,
        }}
      >
        <Space>
          <Typography.Text strong>视频预览</Typography.Text>
          <Tag color="blue" style={{ marginInlineEnd: 0 }}>当前 {count} 张</Tag>
        </Space>
        <Space size={10} wrap>
          <CompactSelectControl
            icon={<PictureOutlined style={iconStyle} />}
            label="宽度"
            ariaLabel="选择预览图宽度"
            disabled={loading}
            value={thumbnailWidth}
            options={WIDTH_OPTIONS}
            selectWidth={92}
            popupWidth={104}
            onChange={handleThumbnailWidthChange}
          />
          <CompactSelectControl
            icon={<PictureOutlined style={iconStyle} />}
            label="质量"
            ariaLabel="选择预览图质量"
            disabled={loading}
            value={jpegQuality}
            options={QUALITY_OPTIONS}
            selectWidth={70}
            popupWidth={90}
            onChange={handleJpegQualityChange}
          />
          <Stepper
            icon={<AppstoreOutlined style={iconStyle} />}
            label={<>每行 <strong style={{ margin: "0 2px" }}>{columns}</strong> 张</>}
            onMinus={handleColumnsDecrement}
            onPlus={handleColumnsIncrement}
            minusDisabled={columns <= MIN_COLUMNS}
            plusDisabled={columns >= MAX_COLUMNS}
            minusTooltip="每行少 1 张"
            plusTooltip="每行多 1 张"
            minusAriaLabel="每行减少 1 张"
            plusAriaLabel="每行增加 1 张"
          />
          <Stepper
            icon={<PictureOutlined style={iconStyle} />}
            label={
              <>
                共
                <Select
                  aria-label="选择预览图数量"
                  className="preview-count-select"
                  disabled={loading}
                  options={countOptions}
                  popupMatchSelectWidth={92}
                  size="small"
                  value={count}
                  variant="borderless"
                  style={{ width: 76, margin: "0 -4px" }}
                  onChange={handleCountChange}
                />
              </>
            }
            onMinus={handleDecrement}
            onPlus={handleIncrement}
            minusDisabled={loading || count <= MIN_COUNT}
            plusDisabled={loading || count >= MAX_COUNT}
            minusTooltip={`减少 ${STEP} 张预览图`}
            plusTooltip={`增加 ${STEP} 张预览图`}
            minusAriaLabel={`减少 ${STEP} 张预览图`}
            plusAriaLabel={`增加 ${STEP} 张预览图`}
          />
        </Space>
      </div>

      <div style={{ flex: 1, overflow: "auto", padding: 16, position: "relative" }}>
        {errorText ? (
          <Alert type="error" showIcon message="预览失败" description={errorText} />
        ) : null}
        {loading && thumbnails.length === 0 ? (
          <div
            style={{
              height: "100%",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
            }}
          >
            <Spin tip="正在抽取缩略图..." size="large" />
          </div>
        ) : null}
        {!loading && !errorText && thumbnails.length === 0 ? (
          <Empty description="暂无缩略图" />
        ) : null}
        {thumbnails.length > 0 ? (
          <div style={{ position: "relative" }}>
            <Image.PreviewGroup
              items={thumbnails.map((thumb) => convertFileSrc(thumb.path))}
              preview={{
                imageRender: renderLargePreviewImage,
                actionsRender: renderPreviewActions,
              }}
            >
              <div
                style={{
                  display: "grid",
                  gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))`,
                  gap: 12,
                }}
              >
                {thumbnails.map((thumb) => (
                  <ThumbnailCard key={thumb.index} thumb={thumb} />
                ))}
              </div>
            </Image.PreviewGroup>
          </div>
        ) : null}
      </div>
    </div>
  );
}

interface StepperProps {
  icon?: ReactNode;
  label: ReactNode;
  onMinus: () => void;
  onPlus: () => void;
  minusDisabled?: boolean;
  plusDisabled?: boolean;
  minusTooltip?: string;
  plusTooltip?: string;
  minusAriaLabel?: string;
  plusAriaLabel?: string;
}

function Stepper({
  icon,
  label,
  onMinus,
  onPlus,
  minusDisabled,
  plusDisabled,
  minusTooltip,
  plusTooltip,
  minusAriaLabel,
  plusAriaLabel,
}: StepperProps) {
  const { token } = theme.useToken();
  const wrapperStyle = buildStepperWrapperStyle(token);
  const buttonStyle = buildStepperButtonStyle(token);
  const buttonDisabledStyle = buildStepperButtonDisabledStyle(token);
  const labelStyle = buildStepperLabelStyle(token);
  return (
    <div style={wrapperStyle}>
      <Tooltip title={minusTooltip}>
        <button
          type="button"
          aria-label={minusAriaLabel}
          onClick={onMinus}
          disabled={minusDisabled}
          style={{
            ...buttonStyle,
            ...(minusDisabled ? buttonDisabledStyle : {}),
          }}
          onMouseEnter={(event) => {
            if (minusDisabled) return;
            event.currentTarget.style.background = token.colorFillTertiary;
            event.currentTarget.style.color = token.colorPrimary;
          }}
          onMouseLeave={(event) => {
            event.currentTarget.style.background = "transparent";
            event.currentTarget.style.color = token.colorTextSecondary;
          }}
        >
          <MinusOutlined />
        </button>
      </Tooltip>
      <div style={labelStyle}>
        {icon}
        <span>{label}</span>
      </div>
      <Tooltip title={plusTooltip}>
        <button
          type="button"
          aria-label={plusAriaLabel}
          onClick={onPlus}
          disabled={plusDisabled}
          style={{
            ...buttonStyle,
            ...(plusDisabled ? buttonDisabledStyle : {}),
          }}
          onMouseEnter={(event) => {
            if (plusDisabled) return;
            event.currentTarget.style.background = token.colorFillTertiary;
            event.currentTarget.style.color = token.colorPrimary;
          }}
          onMouseLeave={(event) => {
            event.currentTarget.style.background = "transparent";
            event.currentTarget.style.color = token.colorTextSecondary;
          }}
        >
          <PlusOutlined />
        </button>
      </Tooltip>
    </div>
  );
}

interface CompactSelectControlProps {
  icon?: ReactNode;
  label: string;
  ariaLabel: string;
  disabled?: boolean;
  value: number;
  options: { value: number; label: string }[];
  selectWidth: number;
  popupWidth: number;
  onChange: (value: number) => void;
}

function CompactSelectControl({
  icon,
  label,
  ariaLabel,
  disabled,
  value,
  options,
  selectWidth,
  popupWidth,
  onChange,
}: CompactSelectControlProps) {
  const { token } = theme.useToken();
  return (
    <div style={buildStepperWrapperStyle(token)}>
      <div style={{ ...buildStepperLabelStyle(token), borderLeft: 0, borderRight: 0 }}>
        {icon}
        <span>{label}</span>
        <Select
          aria-label={ariaLabel}
          className="preview-count-select"
          disabled={disabled}
          options={options}
          popupMatchSelectWidth={popupWidth}
          size="small"
          value={value}
          variant="borderless"
          style={{ width: selectWidth, margin: "0 -4px" }}
          onChange={onChange}
        />
      </div>
    </div>
  );
}

function ThumbnailCard({ thumb }: { thumb: PreviewThumbnail }) {
  const { token } = theme.useToken();
  const [aspectRatio, setAspectRatio] = useState<string>("16 / 9");
  return (
    <div
      style={{
        background: token.colorBgContainer,
        border: `1px solid ${token.colorBorderSecondary}`,
        borderRadius: 8,
        overflow: "hidden",
        boxShadow: token.boxShadowTertiary,
      }}
    >
      <Image
        src={convertFileSrc(thumb.path)}
        alt={`thumbnail-${thumb.index}`}
        wrapperStyle={{ width: "100%", display: "block" }}
        onLoad={(event) => {
          const target = event.currentTarget as HTMLImageElement;
          if (target.naturalWidth > 0 && target.naturalHeight > 0) {
            setAspectRatio(`${target.naturalWidth} / ${target.naturalHeight}`);
          }
        }}
        style={{
          width: "100%",
          display: "block",
          aspectRatio,
          objectFit: "contain",
          background: "#000",
          cursor: "zoom-in",
        }}
      />
      <div
        style={{
          padding: "6px 10px",
          display: "flex",
          justifyContent: "space-between",
          fontSize: 12,
          color: token.colorTextSecondary,
        }}
      >
        <span>#{thumb.index + 1}</span>
        <span>{formatTimestamp(thumb.time_secs)}</span>
      </div>
    </div>
  );
}

function renderLargePreviewImage(originalNode: ReactElement) {
  const imageNode = originalNode as ReactElement<{ style?: CSSProperties }>;
  return cloneElement(imageNode, {
    style: {
      ...imageNode.props.style,
      width: "min(88vw, 1280px)",
      maxHeight: "82vh",
      objectFit: "contain",
    },
  });
}

function renderPreviewActions(
  originalNode: ReactElement,
  info: { current: number; image: { url?: string } }
) {
  const actionsNode = originalNode as ReactElement<{
    children?: ReactNode;
    className?: string;
  }>;
  const rootClassName =
    actionsNode.props.className?.split(" ").find(Boolean) ?? "ant-image-preview-actions";
  const actionClassName = `${rootClassName}-action`;

  return cloneElement(actionsNode, {
    children: (
      <>
        {actionsNode.props.children}
        <button
          type="button"
          className={actionClassName}
          aria-label="download"
          title="下载"
          onClick={(event) => {
            event.stopPropagation();
            void downloadPreviewImage(info.image.url, info.current);
          }}
        >
          <DownloadOutlined />
        </button>
      </>
    ),
  });
}

async function downloadPreviewImage(url: string | undefined, current: number) {
  if (!url) return;
  const filename = `preview-${String(current + 1).padStart(3, "0")}.jpg`;
  try {
    const response = await fetch(url);
    const blob = await response.blob();
    const savedPath = await savePreviewBlob(blob, filename);
    if (savedPath) {
      message.success(`已保存到 ${savedPath}`);
    }
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") {
      return;
    }
    console.debug("Failed to download preview image", error);
    message.error("保存图片失败");
  }
}

async function savePreviewBlob(blob: Blob, filename: string) {
  const directoryPickerWindow = window as DirectoryPickerWindow;
  if (directoryPickerWindow.showDirectoryPicker) {
    const directoryHandle = await directoryPickerWindow.showDirectoryPicker();
    const fileHandle = await directoryHandle.getFileHandle(filename, {
      create: true,
    });
    const writable = await fileHandle.createWritable();
    await writable.write(blob);
    await writable.close();
    return `${directoryHandle.name}/${filename}`;
  }

  const targetPath = await save({
    defaultPath: filename,
    filters: [{ name: "JPEG 图片", extensions: ["jpg", "jpeg"] }],
  });
  if (!targetPath) return null;

  const objectUrl = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = objectUrl;
  link.download = targetPath;
  document.body.appendChild(link);
  link.click();
  link.remove();
  URL.revokeObjectURL(objectUrl);
  return targetPath;
}

function upsertThumbnail(
  thumbnails: PreviewThumbnail[],
  next: PreviewThumbnail
) {
  const withoutCurrent = thumbnails.filter((item) => item.index !== next.index);
  return sortThumbnails([...withoutCurrent, next]);
}

function sortThumbnails(thumbnails: PreviewThumbnail[]) {
  return [...thumbnails].sort((left, right) => left.index - right.index);
}

function clampColumns(columns: number) {
  return Math.min(MAX_COLUMNS, Math.max(MIN_COLUMNS, columns));
}

function clampCount(count: number) {
  return Math.min(MAX_COUNT, Math.max(MIN_COUNT, count));
}

function clampThumbnailWidth(width: number) {
  const optionValues = WIDTH_OPTIONS.map((option) => option.value);
  if (optionValues.includes(width)) return width;
  return DEFAULT_THUMBNAIL_WIDTH;
}

function clampJpegQuality(quality: number) {
  const optionValues = QUALITY_OPTIONS.map((option) => option.value);
  if (optionValues.includes(quality)) return quality;
  return DEFAULT_JPEG_QUALITY;
}

function formatTimestamp(totalSeconds: number) {
  const safe = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(safe / 3600);
  const minutes = Math.floor((safe % 3600) / 60);
  const seconds = safe % 60;
  const pad = (value: number) => value.toString().padStart(2, "0");
  if (hours > 0) {
    return `${hours}:${pad(minutes)}:${pad(seconds)}`;
  }
  return `${pad(minutes)}:${pad(seconds)}`;
}

function formatError(error: unknown): string {
  const text = String(error ?? "").trim();
  if (!text) return "未知错误";
  return text.replace(
    /^(Invalid input|Conversion error|Network error|IO error):\s*/i,
    ""
  );
}
