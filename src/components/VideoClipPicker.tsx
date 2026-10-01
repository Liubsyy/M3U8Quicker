import { t, useTranslation } from "../i18n";
import { useMemo, useRef, useState } from "react";
import { Alert, Button, Slider, Space, Typography } from "antd";
import { convertFileSrc } from "@tauri-apps/api/core";

export interface ClipRange {
  start: number;
  end: number;
}

interface VideoClipPickerProps {
  inputPath?: string;
  value?: ClipRange;
  onChange?: (next: ClipRange) => void;
  onLoadStateChange?: (state: { duration: number; loadFailed: boolean }) => void;
}

function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) {
    return "00:00:00.000";
  }
  const totalMs = Math.round(seconds * 1000);
  const ms = totalMs % 1000;
  const totalSec = Math.floor(totalMs / 1000);
  const s = totalSec % 60;
  const totalMin = Math.floor(totalSec / 60);
  const m = totalMin % 60;
  const h = Math.floor(totalMin / 60);
  return (
    `${String(h).padStart(2, "0")}:` +
    `${String(m).padStart(2, "0")}:` +
    `${String(s).padStart(2, "0")}.` +
    `${String(ms).padStart(3, "0")}`
  );
}

export function VideoClipPicker({
  inputPath,
  value,
  onChange,
  onLoadStateChange,
}: VideoClipPickerProps) {
  useTranslation();
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const [duration, setDuration] = useState(0);
  const [loadFailed, setLoadFailed] = useState(false);
  const lastSliderRef = useRef<[number, number] | null>(null);

  const videoSrc = useMemo(() => {
    if (!inputPath) return "";
    try {
      return convertFileSrc(inputPath);
    } catch {
      return "";
    }
  }, [inputPath]);

  const handleLoadStart = () => {
    setDuration(0);
    setLoadFailed(false);
    onLoadStateChange?.({ duration: 0, loadFailed: false });
    lastSliderRef.current = null;
  };

  const start = value?.start ?? 0;
  const end = value?.end ?? duration;
  const sliderValue: [number, number] = [
    Math.max(0, Math.min(start, duration)),
    Math.max(0, Math.min(end, duration)),
  ];

  const emit = (next: ClipRange) => {
    onChange?.(next);
  };

  const handleLoadedMetadata = () => {
    const video = videoRef.current;
    if (!video) return;
    const d = Number.isFinite(video.duration) ? video.duration : 0;
    setDuration(d);
    setLoadFailed(false);
    onLoadStateChange?.({ duration: d, loadFailed: false });
    emit({ start: 0, end: d });
  };

  const handleError = () => {
    setLoadFailed(true);
    setDuration(0);
    onLoadStateChange?.({ duration: 0, loadFailed: true });
  };

  const seekTo = (time: number) => {
    const video = videoRef.current;
    if (!video) return;
    const clamped = Math.max(0, Math.min(time, duration));
    try {
      video.currentTime = clamped;
    } catch {
      // ignore seek errors before metadata is ready
    }
  };

  const handleSliderChange = (next: number | number[]) => {
    if (!Array.isArray(next) || next.length < 2) return;
    const [ns, ne] = next;
    const previous: [number, number] = lastSliderRef.current ?? sliderValue;
    if (Math.abs(ns - previous[0]) >= Math.abs(ne - previous[1])) {
      seekTo(ns);
    } else {
      seekTo(ne);
    }
    lastSliderRef.current = [ns, ne];
    emit({ start: ns, end: ne });
  };

  const handleSetStart = () => {
    const video = videoRef.current;
    if (!video || duration <= 0) return;
    const next = Math.min(video.currentTime, end);
    emit({ start: next, end });
  };

  const handleSetEnd = () => {
    const video = videoRef.current;
    if (!video || duration <= 0) return;
    const next = Math.max(video.currentTime, start);
    emit({ start, end: next });
  };

  if (!inputPath) {
    return (
      <Alert
        type="info"
        showIcon
        message={t("selectAVideoToClipFirst")}
        description={t("previewSupportsMp4M4vMovWebmOnlyConvertMkvTs")}
      />
    );
  }

  return (
    <Space direction="vertical" size={12} style={{ width: "100%" }}>
      <Typography.Text type="secondary">
        {t("previewSupportsMp4M4vMovWebmOnlyConvertMkvTs2")}</Typography.Text>
      {loadFailed ? (
        <Alert
          type="warning"
          showIcon
          message={t("thisFileCannotBePlayedInThePreview")}
          description={t("useTsToMp4LocalM3u8ToMp4MultiTrack")}
        />
      ) : (
        <video
          ref={videoRef}
          src={videoSrc}
          controls
          preload="metadata"
          onLoadStart={handleLoadStart}
          onLoadedMetadata={handleLoadedMetadata}
          onError={handleError}
          style={{ width: "100%", maxHeight: 360, background: "#000" }}
        />
      )}
      <Slider
        range
        min={0}
        max={duration > 0 ? duration : 1}
        step={0.05}
        value={sliderValue}
        onChange={handleSliderChange}
        disabled={loadFailed || duration <= 0}
        tooltip={{ formatter: (v) => formatTime(typeof v === "number" ? v : 0) }}
      />
      <Space wrap>
        <Button size="small" onClick={handleSetStart} disabled={loadFailed || duration <= 0}>
          {t("setStartCurrentPlaybackPosition")}</Button>
        <Button size="small" onClick={handleSetEnd} disabled={loadFailed || duration <= 0}>
          {t("setEndCurrentPlaybackPosition")}</Button>
      </Space>
      <Space wrap size={[16, 4]}>
        <Typography.Text type="secondary">{t("start")}{formatTime(sliderValue[0])}</Typography.Text>
        <Typography.Text type="secondary">{t("end")}{formatTime(sliderValue[1])}</Typography.Text>
        <Typography.Text type="secondary">
          {t("duration2")}{formatTime(Math.max(0, sliderValue[1] - sliderValue[0]))}
        </Typography.Text>
      </Space>
    </Space>
  );
}
