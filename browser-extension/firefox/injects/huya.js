(function () {
  const TARGET_EVENT = "m3u8quicker:custom-target";
  const CONFIG_POLL_MS = 1000;
  const TOKEN_REFRESH_MS = 4 * 60 * 1000;
  let lastFingerprint = "";
  let lastRefreshAt = Date.now();
  let refreshInFlight = false;

  function tick() {
    const config = window.hyPlayerConfig;
    if (config && config.stream) {
      publishStream(config.stream);
    }
  }

  function publishStream(stream) {
    const room = stream && Array.isArray(stream.data) ? stream.data[0] : null;
    const liveInfo = room && room.gameLiveInfo;
    const lines = room && Array.isArray(room.gameStreamInfoList)
      ? room.gameStreamInfoList
      : [];
    if (!liveInfo || lines.length === 0) {
      return;
    }

    const selected = selectLine(lines, "flv") || selectLine(lines, "hls");
    if (!selected) {
      return;
    }

    const protocol = hasProtocol(selected, "flv") ? "flv" : "hls";
    const baseUrl = buildStreamUrl(selected, protocol);
    if (!baseUrl) {
      return;
    }

    const title = getTitle(liveInfo);
    const qualities = collectQualities(stream.vMultiStreamInfo, baseUrl, title);
    const targetUrl = qualities.length > 0 ? qualities[0].url : withTitle(baseUrl, title);
    const roomId = String(
      liveInfo.profileRoom || liveInfo.uid || location.pathname.split("/").filter(Boolean)[0] || "live"
    );
    const fingerprint = JSON.stringify({ protocol, targetUrl, qualities });
    if (fingerprint === lastFingerprint) {
      return;
    }
    lastFingerprint = fingerprint;

    window.dispatchEvent(
      new CustomEvent(TARGET_EVENT, {
        detail: {
          source: "huya",
          url: targetUrl,
          fileName: title + "." + protocol,
          fileType: protocol,
          isLive: true,
          thumbnail: normalizeUrl(liveInfo.screenshot || ""),
          groupId: "huya:" + roomId + ":" + protocol,
          qualities,
        },
      })
    );
  }

  function selectLine(lines, protocol) {
    return lines
      .filter((line) => hasProtocol(line, protocol))
      .sort((left, right) => priority(right) - priority(left))[0] || null;
  }

  function hasProtocol(line, protocol) {
    if (!line || typeof line !== "object") {
      return false;
    }
    if (protocol === "flv") {
      return Boolean(line.sFlvUrl && line.sStreamName && line.sFlvUrlSuffix);
    }
    return Boolean(line.sHlsUrl && line.sStreamName && line.sHlsUrlSuffix);
  }

  function priority(line) {
    const web = Number(line && line.iWebPriorityRate);
    if (Number.isFinite(web)) {
      return web;
    }
    const pc = Number(line && line.iPCPriorityRate);
    return Number.isFinite(pc) ? pc : 0;
  }

  function buildStreamUrl(line, protocol) {
    const base = protocol === "flv" ? line.sFlvUrl : line.sHlsUrl;
    const suffix = protocol === "flv" ? line.sFlvUrlSuffix : line.sHlsUrlSuffix;
    const antiCode = protocol === "flv" ? line.sFlvAntiCode : line.sHlsAntiCode;
    if (!base || !line.sStreamName || !suffix) {
      return "";
    }
    const query = typeof antiCode === "string" && antiCode ? "?" + antiCode.replace(/^\?/, "") : "";
    return normalizeUrl(
      String(base).replace(/\/+$/, "") + "/" + line.sStreamName + "." + suffix + query
    );
  }

  function collectQualities(items, baseUrl, title) {
    const source = Array.isArray(items) ? items : [];
    const ordered = source
      .map((item, index) => ({
        label: cleanLabel(item && item.sDisplayName) || "清晰度 " + (index + 1),
        bitrate: Number(item && item.iBitRate),
        index,
      }))
      .filter((item) => Number.isFinite(item.bitrate) && item.bitrate >= 0)
      .sort((left, right) => qualityRank(right.bitrate) - qualityRank(left.bitrate) || left.index - right.index);

    if (ordered.length === 0) {
      return [{ url: withTitle(baseUrl, title), label: "默认清晰度" }];
    }

    const seen = new Set();
    const qualities = [];
    ordered.forEach((item) => {
      const qualityUrl = item.bitrate > 0 ? appendRatio(baseUrl, item.bitrate) : baseUrl;
      if (seen.has(qualityUrl)) {
        return;
      }
      seen.add(qualityUrl);
      qualities.push({
        url: withTitle(qualityUrl, title),
        label: item.label,
      });
    });
    return qualities;
  }

  function qualityRank(bitrate) {
    return bitrate === 0 ? Number.MAX_SAFE_INTEGER : bitrate;
  }

  function appendRatio(rawUrl, bitrate) {
    if (/([?&])ratio=\d+/i.test(rawUrl)) {
      return rawUrl.replace(/([?&])ratio=\d+/i, "$1ratio=" + bitrate);
    }
    return rawUrl + (rawUrl.includes("?") ? "&" : "?") + "ratio=" + bitrate;
  }

  function normalizeUrl(rawUrl) {
    if (!rawUrl) {
      return "";
    }
    try {
      const url = new URL(rawUrl, window.location.href);
      if (window.location.protocol === "https:" && url.protocol === "http:") {
        url.protocol = "https:";
      }
      return url.href;
    } catch (error) {
      return String(rawUrl);
    }
  }

  function withTitle(rawUrl, title) {
    try {
      const url = new URL(rawUrl, window.location.href);
      url.searchParams.set("title", title);
      return url.href;
    } catch (error) {
      return rawUrl;
    }
  }

  function getTitle(liveInfo) {
    const pageTitle = String(document.title || "").replace(/[-_]?虎牙直播.*$/i, "");
    return cleanTitle(pageTitle) || cleanTitle(liveInfo.nick) || "huya-live";
  }

  function cleanTitle(text) {
    return String(text || "")
      .replace(/[<>:"/\\|?*\u0000-\u001f]/g, "_")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 100);
  }

  function cleanLabel(text) {
    return String(text || "").replace(/\s+/g, " ").trim().slice(0, 40);
  }

  async function refreshFromPage() {
    if (refreshInFlight || Date.now() - lastRefreshAt < TOKEN_REFRESH_MS) {
      return;
    }
    refreshInFlight = true;
    try {
      const response = await fetch(window.location.href, {
        credentials: "include",
        cache: "no-store",
        headers: { Accept: "text/html" },
      });
      if (!response.ok) {
        return;
      }
      const stream = parseStream(await response.text());
      if (stream) {
        publishStream(stream);
      }
    } catch (error) {
      console.debug("[m3u8quicker] huya stream refresh failed", error);
    } finally {
      lastRefreshAt = Date.now();
      refreshInFlight = false;
    }
  }

  function parseStream(html) {
    const line = String(html || "")
      .split(/\r?\n/)
      .find((item) => /^\s*stream:\s*\{/.test(item));
    if (!line) {
      return null;
    }
    try {
      return JSON.parse(line.replace(/^\s*stream:\s*/, "").replace(/,\s*$/, ""));
    } catch (error) {
      console.debug("[m3u8quicker] failed to parse huya player config", error);
      return null;
    }
  }

  tick();
  window.setInterval(tick, CONFIG_POLL_MS);
  window.setInterval(refreshFromPage, TOKEN_REFRESH_MS);
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) {
      tick();
      void refreshFromPage();
    }
  });
})();
