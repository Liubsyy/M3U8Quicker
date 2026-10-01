import { currentLanguage, renderMessage, translatedMessage, type TranslatedMessage, t, useTranslation } from "../i18n";
import { useEffect, useEffectEvent, useMemo, useRef, useState } from "react";
import { Alert, Spin } from "antd";
import type Hls from "hls.js";
import type Mpegts from "mpegts.js";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  DownloadProgressEvent,
  DownloadStatus,
  LiveProgressEvent,
  PlaybackSourceKind,
} from "../types";
import { liveRecordStatusToDownloadStatus } from "../types";
import {
  closeDownloadPlaybackSession,
  closeLivePlaybackSession,
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
  isLive: boolean;
  scope: "download" | "live";
  initialStatus: DownloadStatus | null;
}

export function PlaybackWindow() {
  const { i18n: { language } } = useTranslation();
  const query = useMemo(() => parsePlaybackWindowQuery(window.location.search), []);
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const hlsRef = useRef<Hls | null>(null);
  const mpegtsRef = useRef<Mpegts.Player | null>(null);
  const sessionClosedRef = useRef(false);
  const taskStatusRef = useRef<DownloadStatus | null>(null);
  const lastPrioritizedRef = useRef<{ position: number; at: number } | null>(null);
  const suppressSeekGuardRef = useRef(false);
  const [taskStatus, setTaskStatus] = useState<DownloadStatus | null>(
    query?.initialStatus ?? null
  );
  const [loading, setLoading] = useState(true);
  const [errorText, setErrorText] = useState<TranslatedMessage | null>(
    query ? null : translatedMessage("playerParametersAreIncompleteThisTaskCannotBeOpened")
  );
  const [noticeText, setNoticeText] = useState<TranslatedMessage | null>(null);

  useEffect(() => {
    if (!query) return;
    const title = t("playing", { value0: query.filename });
    document.title = title;
    void getCurrentWebviewWindow().setTitle(title).catch(console.error);
  }, [query, language]);

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


    appendDebugLog(t("startingTaskStatusSyncFilename", { value0: query.filename }));

    let disposed = false;
    let unlisten: UnlistenFn | undefined;

    if (query.scope === "live") {
      // Live recordings are not in the download table; follow live-progress
      // events for status. Finished recordings keep the initial status.
      setLoading(false);
      listen<LiveProgressEvent>("live-progress", (event) => {
        if (event.payload.id !== query.taskId) {
          return;
        }
        appendDebugLog(t("liveProgressEventStatus", { value0: String(event.payload.status) }));
        setTaskStatus(liveRecordStatusToDownloadStatus(event.payload.status));
      }).then((fn) => {
        if (disposed) {
          fn();
          return;
        }
        unlisten = fn;
      });

      return () => {
        disposed = true;
        unlisten?.();
      };
    }

    const syncTask = async () => {
      try {
        const task = await getDownloadSummary(query.taskId);
        if (disposed) {
          return;
        }

        appendDebugLog(t("taskStatusSyncedStatus", { value0: formatStatus(task.status) }));
        setTaskStatus(task.status);
      } catch (error) {
        if (!disposed) {
          console.error("Failed to sync playback task", error);
          appendDebugLog(t("taskStatusSyncFailed", { value0: String(error) }));
          setErrorText(translatedMessage("theDownloadTaskWasDeletedPlaybackResourcesAreUnavailable"));
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

      appendDebugLog(t("downloadProgressEventStatus", { value0: formatStatus(event.payload.status) }));
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
      appendDebugLog(t("closingPlaybackSession"));
      const closeSessionFn =
        query.scope === "live"
          ? closeLivePlaybackSession
          : closeDownloadPlaybackSession;
      void closeSessionFn(query.taskId, query.sessionToken).catch((error) => {
        console.debug("Failed to close playback session", error);
        appendDebugLog(t("failedToClosePlaybackSession", { value0: String(error) }));
      });
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
      if (query.scope !== "download" || query.playbackKind !== "hls") {
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
        appendDebugLog(t("requestingPriorityDownloadCurrenttime", { value0: currentPosition.toFixed(3) }));
        await prioritizeDownloadPlaybackPosition(query.taskId, currentPosition);
      } catch (error) {
        console.debug("Failed to prioritize playback segment", error);
        appendDebugLog(t("priorityDownloadRequestFailed", { value0: String(error) }));
      }
    };

    const handlePlaying = () => {
      if (disposed) {
        return;
      }
      appendDebugLog("video: playing");
      setNoticeText(null);
      setErrorText((current) => {
        if (current?.key === "videoStreamUnavailablePleaseTryAgainLater") {
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
        if (suppressSeekGuardRef.current) {
          suppressSeekGuardRef.current = false;
          return;
        }

        if (isInProgressProgressiveFilePlayback(query, taskStatusRef.current)) {
          const bufferedEnd = getPlayableBufferedEnd(video);
          if (bufferedEnd !== null && video.currentTime > bufferedEnd + 0.35) {
            const fallbackTime = Math.max(0, bufferedEnd - 0.1);
            suppressSeekGuardRef.current = true;
            setNoticeText(translatedMessage("thisPositionHasNotBeenDownloadedYetPlayWithinThe"));
            appendDebugLog(
              `video: seek blocked target=${video.currentTime.toFixed(3)} bufferedEnd=${bufferedEnd.toFixed(3)}`
            );
            video.currentTime = fallbackTime;
            return;
          }
        }

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
        setErrorText(translatedMessage("videoStreamUnavailablePleaseTryAgainLaterMediaErrorCode", { value0: mediaError.code }));
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

    if (query.playbackKind === "flv" || query.playbackKind === "mpegts") {
      const mediaType = query.playbackKind === "mpegts" ? "mpegts" : "flv";
      void (async () => {
        appendDebugLog(t("loadingMpegtsJsOnDemandType", { value0: mediaType }));
        const mpegtsModule = (await import("mpegts.js")).default;
        if (disposed) {
          return;
        }

        if (!mpegtsModule.isSupported()) {
          appendDebugLog(t("mpegtsJsReportsThatIsUnsupported", { value0: mediaType }));
          setErrorText(
            mediaType === "mpegts"
              ? translatedMessage("softwareDecodingIsNotSupportedForThisVideoInThe")
              : translatedMessage("flvPlaybackIsNotSupportedInTheCurrentEnvironment")
          );
          return;
        }

        appendDebugLog(t("mpegtsJsLoadedCreatingPlayerTypeIslive", { value0: mediaType, value1: query.isLive }));
        const player = mpegtsModule.createPlayer(
          { type: mediaType, isLive: query.isLive, url: query.playbackUrl },
          { enableWorker: true, liveBufferLatencyChasing: query.isLive }
        );
        mpegtsRef.current = player;
        player.attachMediaElement(video);
        player.on(mpegtsModule.Events.ERROR, (type, detail) => {
          console.error("mpegts playback error", type, detail);
          appendDebugLog(
            `mpegts: error type=${String(type)} detail=${String(detail)}`
          );
          setErrorText(
            translatedMessage("videoStreamUnavailablePleaseTryAgainLater2", { value0: String(detail ?? type) })
          );
        });
        player.load();
        setLoading(false);
      })().catch((error) => {
        console.error("Failed to load mpegts.js", error);
        appendDebugLog(t("failedToLoadMpegtsJs", { value0: String(error) }));
        setErrorText(translatedMessage("failedToInitializePlayerCloseThisWindowAndTryAgain"));
      });
    } else if (query.playbackKind === "file") {
      appendDebugLog(t("usingDirectFilePlayback"));
      video.src = query.playbackUrl;
      video.load();
      setLoading(false);
    } else if (!query.isLive && video.canPlayType("application/vnd.apple.mpegurl")) {
      appendDebugLog(t("usingNativeHlsPlayback"));
      video.src = query.playbackUrl;
      video.load();
      try {
        video.currentTime = 0;
      } catch (error) {
        appendDebugLog(t("failedToSetNativeHlsStartPosition", { value0: String(error) }));
      }
    } else {
      void (async () => {
        appendDebugLog(query.isLive ? t("usingHlsJsForLiveHls") : t("loadingHlsJsOnDemand"));
        const { default: HlsConstructor } = await import("hls.js");
        if (disposed) {
          return;
        }

        if (!HlsConstructor.isSupported()) {
          appendDebugLog(t("hlsJsReportsThatHlsIsUnsupported"));
          setErrorText(translatedMessage("hlsPlaybackIsNotSupportedInTheCurrentEnvironment"));
          return;
        }

        appendDebugLog(t("hlsJsLoadedAttachingMedia"));
        const hls = new HlsConstructor({
          enableWorker: true,
          startPosition: 0,
        });
        hlsRef.current = hls;
        hls.loadSource(query.playbackUrl);
        hls.attachMedia(video);
        hls.on(HlsConstructor.Events.MANIFEST_PARSED, () => {
          appendDebugLog(t("hlsManifestParsedStartingAt0Seconds"));
          try {
            video.currentTime = 0;
          } catch (error) {
            appendDebugLog(t("failedToSetHlsStartPosition", { value0: String(error) }));
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
              translatedMessage("videoStreamRequestFailed", { value0: data.details, value1: "response" in data && data.response?.code ? `（HTTP ${data.response.code}）` : "" })
            );
            appendDebugLog(
              `hls: network fatal details=${data.details}${"response" in data && data.response?.code ? ` http=${data.response.code}` : ""}`
            );
            hls.startLoad();
            return;
          }

          if (data.type === HlsConstructor.ErrorTypes.MEDIA_ERROR) {
            appendDebugLog(t("hlsMediaErrorTryingRecovermediaerror"));
            hls.recoverMediaError();
            return;
          }

          setErrorText(translatedMessage("failedToInitializePlayer", { value0: data.details }));
        });
      })().catch((error) => {
        console.error("Failed to load hls.js", error);
        appendDebugLog(t("failedToLoadHlsJs", { value0: String(error) }));
        setErrorText(translatedMessage("failedToInitializePlayerCloseThisWindowAndTryAgain"));
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
      if (mpegtsRef.current) {
        try {
          mpegtsRef.current.destroy();
        } catch (error) {
          console.debug("Failed to destroy mpegts player", error);
        }
        mpegtsRef.current = null;
      }
      hlsRef.current?.destroy();
      hlsRef.current = null;
      video.removeAttribute("src");
      video.load();
    };
  }, [query]);

  if (!query) {
    return (
      <div style={containerStyle}>
        <Alert type="error" message={renderMessage(errorText)} showIcon />
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
            <Spin tip={t("loadingPlayer")} />
          </div>
        ) : null}

        <div style={alertsOverlayStyle}>
          {failedMessage ? (
            <Alert type="error" showIcon message={t("downloadFailed")} description={failedMessage} />
          ) : null}
          {taskStatus === "Cancelled" ? (
            <Alert type="warning" showIcon message={t("downloadCancelledNoMoreSegmentsWillBeDownloadedForPlayback")} />
          ) : null}
          {noticeText ? <Alert type="warning" showIcon message={renderMessage(noticeText)} /> : null}
          {errorText ? <Alert type="error" showIcon message={renderMessage(errorText)} /> : null}
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
  const filename = params.get("filename")?.trim() || t("playing2");
  const scope = params.get("scope")?.trim() === "live" ? "live" : "download";
  const isLive = params.get("isLive")?.trim() === "1";
  const initialStatus = parseStatusParam(params.get("status"));

  if (!taskId || !playbackUrl || !sessionToken) {
    return null;
  }

  return {
    taskId,
    playbackUrl,
    playbackKind,
    sessionToken,
    filename,
    scope,
    isLive,
    initialStatus,
  };
}

function normalizePlaybackKind(value: string | null): PlaybackSourceKind {
  if (value === "file") return "file";
  if (value === "flv") return "flv";
  if (value === "mpegts") return "mpegts";
  return "hls";
}

function parseStatusParam(value: string | null): DownloadStatus | null {
  const trimmed = value?.trim();
  if (!trimmed) {
    return null;
  }
  if (trimmed === "Failed") {
    return { Failed: "" };
  }
  return trimmed as DownloadStatus;
}

function isInProgressProgressiveFilePlayback(
  query: PlaybackWindowQuery,
  taskStatus: DownloadStatus | null
) {
  return (
    query.playbackKind === "file" &&
    supportsProgressivePlaybackFilename(query.filename) &&
    (taskStatus === "Downloading" || taskStatus === "Paused")
  );
}

function supportsProgressivePlaybackFilename(filename: string) {
  const lower = filename.trim().toLowerCase();
  return lower.endsWith(".mp4") || lower.endsWith(".webm");
}

function getPlayableBufferedEnd(video: HTMLVideoElement) {
  if (video.buffered.length === 0) {
    return null;
  }

  const currentTime = video.currentTime || 0;
  for (let index = 0; index < video.buffered.length; index += 1) {
    const start = video.buffered.start(index);
    const end = video.buffered.end(index);
    if (currentTime >= start && currentTime <= end) {
      return end;
    }
  }

  return video.buffered.end(video.buffered.length - 1);
}

function formatDebugTime() {
  return new Date().toLocaleTimeString(currentLanguage(), {
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
