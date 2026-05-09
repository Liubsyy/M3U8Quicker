(function () {
  const EVENT_NAME = "m3u8quicker:custom-manifest";
  const SOURCE_ID = "bilibili";
  const FORMAT = "m3u8quicker-dash-v1";
  let lastSignature = "";

  function readPlayInfo() {
    let info = window.__playinfo__;
    if (typeof info === "string") {
      try {
        info = JSON.parse(info);
      } catch {
        return null;
      }
    }
    if (!info || typeof info !== "object") {
      return null;
    }
    const data = info.data && typeof info.data === "object" ? info.data : info;
    const dash = data.dash && typeof data.dash === "object" ? data.dash : null;
    if (!dash || !Array.isArray(dash.video) || dash.video.length === 0) {
      return null;
    }
    return { data, dash };
  }

  function pickUrl(track) {
    return track.baseUrl || track.base_url || "";
  }

  function codecLabel(codecs) {
    const value = String(codecs || "").toLowerCase();
    if (value.startsWith("avc1")) return "AVC";
    if (value.startsWith("hev1") || value.startsWith("hvc1")) return "HEVC";
    if (value.startsWith("av01")) return "AV1";
    if (value.startsWith("mp4a")) return "AAC";
    return codecs || "codec";
  }

  function toTrack(track, type, index, duration) {
    const url = pickUrl(track);
    if (!url) {
      return null;
    }

    const id = `${type}-${index}-${track.id || "0"}-${track.codecid || codecLabel(track.codecs)}`;
    const resolution =
      type === "video" && track.width && track.height
        ? `${track.width}x${track.height}`
        : undefined;
    const label =
      type === "video"
        ? `${track.height || track.id || "video"}P · ${codecLabel(track.codecs)} · ${Math.round((track.bandwidth || 0) / 1000)} kbps`
        : `${codecLabel(track.codecs)} · ${Math.round((track.bandwidth || 0) / 1000)} kbps`;

    return {
      id,
      label,
      bandwidth: track.bandwidth || undefined,
      resolution,
      codecs: track.codecs || undefined,
      language: type === "audio" ? "und" : undefined,
      segments: [
        {
          uri: url,
          duration,
        },
      ],
    };
  }

  function buildManifests() {
    const parsed = readPlayInfo();
    if (!parsed) {
      return [];
    }
    const { data, dash } = parsed;
    const duration =
      Number(dash.duration || data.timelength / 1000 || 0) || 1;
    const audios = Array.isArray(dash.audio) ? dash.audio : [];
    const bestAudio = audios.reduce(
      (best, candidate) =>
        !best || (candidate.bandwidth || 0) > (best.bandwidth || 0) ? candidate : best,
      null
    );
    const pageTitle = document.title || "bilibili";

    const manifests = [];
    dash.video.forEach((videoTrack, videoIndex) => {
      const video = toTrack(videoTrack, "video", videoIndex, duration);
      if (!video) {
        return;
      }
      const audio = bestAudio ? toTrack(bestAudio, "audio", videoIndex, duration) : null;

      const qualityLabel = videoTrack.height
        ? `${videoTrack.height}P`
        : videoTrack.id
          ? `q${videoTrack.id}`
          : `v${videoIndex + 1}`;
      const codecPart = codecLabel(videoTrack.codecs);
      const title = `${pageTitle} - ${qualityLabel} ${codecPart}`;

      manifests.push({
        title,
        manifest: {
          format: FORMAT,
          title,
          base_url: window.location.href,
          tracks: {
            video: [video],
            audio: audio ? [audio] : [],
          },
          default_selection: {
            video_id: video.id,
            audio_id: audio ? audio.id : undefined,
          },
        },
      });
    });

    return manifests;
  }

  function emitManifests() {
    const manifests = buildManifests();
    if (manifests.length === 0) {
      return;
    }
    const payloads = manifests.map(({ title, manifest }) => ({
      title,
      manifestJson: JSON.stringify(manifest),
    }));
    const signature = payloads.map((item) => item.manifestJson).join("|");
    if (signature === lastSignature) {
      return;
    }
    lastSignature = signature;
    payloads.forEach(({ title, manifestJson }) => {
      window.dispatchEvent(
        new CustomEvent(EVENT_NAME, {
          detail: {
            source: SOURCE_ID,
            title,
            manifest: manifestJson,
          },
        })
      );
    });
  }

  emitManifests();
  window.setInterval(emitManifests, 2000);
})();
