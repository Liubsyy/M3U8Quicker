import { useEffect, useEffectEvent, useMemo, useRef, useState } from "react";
import { Alert, Spin } from "antd";
import type Hls from "hls.js";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  DownloadProgressEvent,
  DownloadStatus,
  PlaybackSourceKind,
} from "../types";
import {
  closeDownloadPlaybackSession,
  getDownloadSummary,
  prioritizeDownloadPlaybackPosition,
} from "../services/api";

const PLAYBACK_VOLUME_STORAGE_KEY = "m3u8quicker.playbackVolume";
const PLAYBACK_MUTED_STORAGE_KEY = "m3u8quicker.playbackMuted";

interface PlaybackWindowQuery {
  taskId: string;
  playbackUrl: string;
  playbackKind: PlaybackSourceKind;
  sessionToken: string;
  filename: string;
}

export function PlaybackWindow() {
  const query = useMemo(() => parsePlaybackWindowQuery(window.location.search), []);
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const hlsRef = useRef<Hls | null>(null);
  const sessionClosedRef = useRef(false);
  const taskStatusRef = useRef<DownloadStatus | null>(null);
  const lastPrioritizedRef = useRef<{ position: number; at: number } | null>(null);
  const [taskStatus, setTaskStatus] = useState<DownloadStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [errorText, setErrorText] = useState<string | null>(
    query ? null : "播放器参数不完整，无法打开当前任务。"
  );

  const appendDebugLog = useEffectEvent((message: string) => {
    const line = `${formatDebugTime()} ${message}`;
    console.info(`[playback-ui] ${line}`);
  });

  useEffect(() => {
    taskStatusRef.current = taskStatus;
  }, [taskStatus]);

  useEffect(() => {
    const htmlStyle = document.documentElement.style;
    const bodyStyle = document.body.style;
    const previousHtmlOverflow = htmlStyle.overflow;
    const previousHtmlHeight = htmlStyle.height;
    const previousBodyOverflow = bodyStyle.overflow;
    const previousBodyHeight = bodyStyle.height;
    const previousBodyMargin = bodyStyle.margin;
    const previousBodyOverscrollBehavior = bodyStyle.overscrollBehavior;

    htmlStyle.overflow = "hidden";
    htmlStyle.height = "100%";
    bodyStyle.overflow = "hidden";
    bodyStyle.height = "100%";
    bodyStyle.margin = "0";
    bodyStyle.overscrollBehavior = "none";

    const preventWheel = (event: WheelEvent) => {
      event.preventDefault();
    };

    window.addEventListener("wheel", preventWheel, { passive: false });

    return () => {
      window.removeEventListener("wheel", preventWheel);
      htmlStyle.overflow = previousHtmlOverflow;
      htmlStyle.height = previousHtmlHeight;
      bodyStyle.overflow = previousBodyOverflow;
      bodyStyle.height = previousBodyHeight;
      bodyStyle.margin = previousBodyMargin;
      bodyStyle.overscrollBehavior = previousBodyOverscrollBehavior;
    };
  }, []);

  useEffect(() => {
    if (!query) {
      return;
    }

    document.title = `播放中 - ${query.filename}`;
    appendDebugLog(`开始同步任务状态 filename=${query.filename}`);

    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    const syncTask = async () => {
      try {
        const task = await getDownloadSummary(query.taskId);
        if (disposed) {
          return;
        }

        appendDebugLog(`同步任务状态成功 status=${formatStatus(task.status)}`);
        setTaskStatus(task.status);
      } catch (error) {
        if (!disposed) {
          console.error("Failed to sync playback task", error);
          appendDebugLog(`同步任务状态异常: ${String(error)}`);
          setErrorText("下载任务已删除，播放资源不可用。");
        }
      } finally {
        if (!disposed) {
          setLoading(false);
        }
      }
    };

    void syncTask();
    const intervalId = window.setInterval(() => {
      void syncTask();
    }, 2000);

    listen<DownloadProgressEvent>("download-progress", (event) => {
      if (event.payload.id !== query.taskId) {
        return;
      }

      appendDebugLog(`收到下载进度事件 status=${formatStatus(event.payload.status)}`);
      setTaskStatus(event.payload.status);
      setLoading(false);
    }).then((fn) => {
      if (disposed) {
        fn();
        return;
      }
      unlisten = fn;
    });

    return () => {
      disposed = true;
      window.clearInterval(intervalId);
      unlisten?.();
    };
  }, [query]);

  useEffect(() => {
    if (!query) {
      return;
    }

    let disposed = false;
    const closeSession = () => {
      if (disposed || sessionClosedRef.current) {
        return;
      }
      sessionClosedRef.current = true;
      appendDebugLog("准备关闭播放会话");
      void closeDownloadPlaybackSession(query.taskId, query.sessionToken).catch(
        (error) => {
          console.debug("Failed to close playback session", error);
          appendDebugLog(`关闭播放会话失败: ${String(error)}`);
        }
      );
    };

    const handleBeforeUnload = () => {
      closeSession();
    };
    window.addEventListener("beforeunload", handleBeforeUnload);
    window.addEventListener("pagehide", handleBeforeUnload);
    window.addEventListener("unload", handleBeforeUnload);
    return () => {
      disposed = true;
      window.removeEventListener("beforeunload", handleBeforeUnload);
      window.removeEventListener("pagehide", handleBeforeUnload);
      window.removeEventListener("unload", handleBeforeUnload);
    };
  }, [query]);

  useEffect(() => {
    if (!query || !videoRef.current) {
      return;
    }

    const video = videoRef.current;
    let disposed = false;

    applySavedVolume(video);

    const prioritizeCurrentPosition = async () => {
      if (query.playbackKind !== "hls") {
        return;
      }

      const currentPosition = video.currentTime || 0;
      const now = Date.now();
      const previous = lastPrioritizedRef.current;
      if (
        previous &&
        Math.abs(previous.position - currentPosition) < 1 &&
        now - previous.at < 800
      ) {
        return;
      }

      lastPrioritizedRef.current = {
        position: currentPosition,
        at: now,
      };

      try {
        appendDebugLog(`请求优先下载 currentTime=${currentPosition.toFixed(3)}`);
        await prioritizeDownloadPlaybackPosition(query.taskId, currentPosition);
      } catch (error) {
        console.debug("Failed to prioritize playback segment", error);
        appendDebugLog(`优先下载请求失败: ${String(error)}`);
      }
    };

    const handlePlaying = () => {
      if (disposed) {
        return;
      }
      appendDebugLog("video: playing");
      setErrorText((current) => {
        if (current === "视频流暂不可用，请稍后重试。") {
          return null;
        }
        return current;
      });
    };
    const handlePause = () => {
      if (!disposed && !video.ended) {
        appendDebugLog("video: pause");
      }
    };
    const handleEnded = () => {
      if (!disposed) {
        appendDebugLog("video: ended");
      }
    };
    const handleSeeking = () => {
      if (!disposed) {
        appendDebugLog(`video: seeking target=${video.currentTime.toFixed(3)}`);
        void prioritizeCurrentPosition();
      }
    };
    const handleSeeked = () => {
      if (!disposed) {
        appendDebugLog(`video: seeked current=${video.currentTime.toFixed(3)}`);
      }
    };
    const handleWaiting = () => {
      if (!disposed) {
        appendDebugLog(`video: waiting current=${video.currentTime.toFixed(3)}`);
        void prioritizeCurrentPosition();
      }
    };
    const handleStalled = () => {
      if (!disposed) {
        appendDebugLog(`video: stalled current=${video.currentTime.toFixed(3)}`);
        void prioritizeCurrentPosition();
      }
    };
    const handleVideoError = () => {
      if (disposed) {
        return;
      }
      const mediaError = video.error;
      if (mediaError) {
        appendDebugLog(`video: error mediaCode=${mediaError.code}`);
        setErrorText(`视频流暂不可用，请稍后重试。媒体错误码：${mediaError.code}`);
      }
    };
    const handleVolumeChange = () => {
      saveVolumeState(video);
    };

    video.addEventListener("playing", handlePlaying);
    video.addEventListener("pause", handlePause);
    video.addEventListener("ended", handleEnded);
    video.addEventListener("seeking", handleSeeking);
    video.addEventListener("seeked", handleSeeked);
    video.addEventListener("waiting", handleWaiting);
    video.addEventListener("stalled", handleStalled);
    video.addEventListener("error", handleVideoError);
    video.addEventListener("volumechange", handleVolumeChange);

    if (query.playbackKind === "file") {
      appendDebugLog("使用最终文件直接播放");
      video.src = query.playbackUrl;
      video.load();
      setLoading(false);
    } else if (video.canPlayType("application/vnd.apple.mpegurl")) {
      appendDebugLog("使用原生 HLS 播放");
      video.src = query.playbackUrl;
      video.load();
      try {
        video.currentTime = 0;
      } catch (error) {
        appendDebugLog(`原生 HLS 设置初始位置失败: ${String(error)}`);
      }
    } else {
      void (async () => {
        appendDebugLog("准备按需加载 hls.js");
        const { default: HlsConstructor } = await import("hls.js");
        if (disposed) {
          return;
        }

        if (!HlsConstructor.isSupported()) {
          appendDebugLog("hls.js 报告当前环境不支持 HLS");
          setErrorText("当前环境不支持 HLS 播放。");
          return;
        }

        appendDebugLog("hls.js 已加载，开始 attach media");
        const hls = new HlsConstructor({
          enableWorker: true,
          startPosition: 0,
        });
        hlsRef.current = hls;
        hls.loadSource(query.playbackUrl);
        hls.attachMedia(video);
        hls.on(HlsConstructor.Events.MANIFEST_PARSED, () => {
          appendDebugLog("hls: manifest parsed，强制从 0 秒开始");
          try {
            video.currentTime = 0;
          } catch (error) {
            appendDebugLog(`hls 设置初始位置失败: ${String(error)}`);
          }
          setLoading(false);
        });
        hls.on(HlsConstructor.Events.LEVEL_LOADED, (_, data) => {
          appendDebugLog(`hls: level loaded fragments=${data.details.fragments.length}`);
        });
        hls.on(HlsConstructor.Events.FRAG_LOADING, (_, data) => {
          appendDebugLog(`hls: frag loading sn=${String(data.frag.sn)}`);
        });
        hls.on(HlsConstructor.Events.FRAG_LOADED, (_, data) => {
          appendDebugLog(`hls: frag loaded sn=${String(data.frag.sn)}`);
        });
        hls.on(HlsConstructor.Events.ERROR, (_, data) => {
          console.error("HLS playback error", data);
          appendDebugLog(
            `hls: error fatal=${String(data.fatal)} type=${data.type} details=${data.details}`
          );
          if (!data.fatal) {
            return;
          }

          if (data.type === HlsConstructor.ErrorTypes.NETWORK_ERROR) {
            setErrorText(
              `视频流网络请求失败：${data.details}${"response" in data && data.response?.code ? `（HTTP ${data.response.code}）` : ""}`
            );
            appendDebugLog(
              `hls: network fatal details=${data.details}${"response" in data && data.response?.code ? ` http=${data.response.code}` : ""}`
            );
            hls.startLoad();
            return;
          }

          if (data.type === HlsConstructor.ErrorTypes.MEDIA_ERROR) {
            appendDebugLog("hls: media error，尝试 recoverMediaError");
            hls.recoverMediaError();
            return;
          }

          setErrorText(`播放器初始化失败：${data.details}`);
        });
      })().catch((error) => {
        console.error("Failed to load hls.js", error);
        appendDebugLog(`加载 hls.js 失败: ${String(error)}`);
        setErrorText("播放器初始化失败，请关闭后重试。");
      });
    }

    return () => {
      disposed = true;
      video.pause();
      video.removeEventListener("playing", handlePlaying);
      video.removeEventListener("pause", handlePause);
      video.removeEventListener("ended", handleEnded);
      video.removeEventListener("seeking", handleSeeking);
      video.removeEventListener("seeked", handleSeeked);
      video.removeEventListener("waiting", handleWaiting);
      video.removeEventListener("stalled", handleStalled);
      video.removeEventListener("error", handleVideoError);
      video.removeEventListener("volumechange", handleVolumeChange);
      hlsRef.current?.destroy();
      hlsRef.current = null;
      video.removeAttribute("src");
      video.load();
    };
  }, [query]);

  if (!query) {
    return (
      <div style={containerStyle}>
        <Alert type="error" message={errorText} showIcon />
      </div>
    );
  }

  const failedMessage =
    taskStatus && typeof taskStatus === "object" && "Failed" in taskStatus
      ? taskStatus.Failed
      : null;
  return (
    <div style={containerStyle}>
      <div style={playerViewportStyle}>
        <video
          ref={videoRef}
          controls
          autoPlay
          playsInline
          style={videoStyle}
        />

        {loading ? (
          <div style={centerOverlayStyle}>
            <Spin tip="正在加载播放器..." />
          </div>
        ) : null}

        <div style={alertsOverlayStyle}>
          {failedMessage ? (
            <Alert type="error" showIcon message="下载失败" description={failedMessage} />
          ) : null}
          {taskStatus === "Cancelled" ? (
            <Alert type="warning" showIcon message="下载已取消，播放器不会再补齐新切片。" />
          ) : null}
          {errorText ? <Alert type="error" showIcon message={errorText} /> : null}
        </div>
      </div>
    </div>
  );
}

function parsePlaybackWindowQuery(search: string): PlaybackWindowQuery | null {
  const params = new URLSearchParams(search);
  const taskId = params.get("taskId")?.trim() || "";
  const playbackUrl = params.get("playbackUrl")?.trim() || "";
  const playbackKind = normalizePlaybackKind(params.get("playbackKind"));
  const sessionToken = params.get("sessionToken")?.trim() || "";
  const filename = params.get("filename")?.trim() || "正在播放";

  if (!taskId || !playbackUrl || !sessionToken) {
    return null;
  }

  return {
    taskId,
    playbackUrl,
    playbackKind,
    sessionToken,
    filename,
  };
}

function normalizePlaybackKind(value: string | null): PlaybackSourceKind {
  return value === "file" ? "file" : "hls";
}

function formatDebugTime() {
  return new Date().toLocaleTimeString("zh-CN", {
    hour12: false,
  });
}

function formatStatus(status: DownloadStatus) {
  if (typeof status === "object" && "Failed" in status) {
    return `Failed(${status.Failed})`;
  }
  return String(status);
}

function applySavedVolume(video: HTMLVideoElement) {
  try {
    const savedVolume = window.localStorage.getItem(PLAYBACK_VOLUME_STORAGE_KEY);
    const savedMuted = window.localStorage.getItem(PLAYBACK_MUTED_STORAGE_KEY);

    if (savedVolume !== null) {
      const volume = Number(savedVolume);
      if (!Number.isNaN(volume)) {
        video.volume = Math.min(Math.max(volume, 0), 1);
      }
    }

    if (savedMuted !== null) {
      video.muted = savedMuted === "true";
    }
  } catch (error) {
    console.debug("Failed to restore playback volume", error);
  }
}

function saveVolumeState(video: HTMLVideoElement) {
  try {
    window.localStorage.setItem(
      PLAYBACK_VOLUME_STORAGE_KEY,
      String(video.volume)
    );
    window.localStorage.setItem(
      PLAYBACK_MUTED_STORAGE_KEY,
      String(video.muted)
    );
  } catch (error) {
    console.debug("Failed to persist playback volume", error);
  }
}

const containerStyle: React.CSSProperties = {
  position: "fixed",
  inset: 0,
  overflow: "hidden",
  background: "#000",
};

const playerViewportStyle: React.CSSProperties = {
  position: "relative",
  width: "100vw",
  height: "100vh",
  overflow: "hidden",
  background: "#000",
};

const videoStyle: React.CSSProperties = {
  display: "block",
  width: "100%",
  height: "100%",
  objectFit: "contain",
  background: "#000",
};

const centerOverlayStyle: React.CSSProperties = {
  position: "absolute",
  inset: 0,
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  pointerEvents: "none",
  zIndex: 2,
};

const alertsOverlayStyle: React.CSSProperties = {
  position: "absolute",
  top: 56,
  left: 14,
  right: 14,
  display: "flex",
  flexDirection: "column",
  gap: 10,
  zIndex: 2,
  pointerEvents: "none",
};
