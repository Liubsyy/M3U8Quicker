(() => {
  const DETECT_EVENT = "m3u8quicker:detected";
  const CUSTOM_MANIFEST_EVENT = "m3u8quicker:custom-manifest";
  const CUSTOM_TARGET_EVENT = "m3u8quicker:custom-target";
  const BACKGROUND_DETECT_MESSAGE = "m3u8quicker:network-detected";
  const FRAME_DETECT_MESSAGE = "m3u8quicker:frame-detected";
  const SYNC_DETECTIONS_MESSAGE = "m3u8quicker:sync-detections";
  const BUTTON_ID = "m3u8quicker-download-button";
  const PANEL_ID = "m3u8quicker-download-panel";
  const ROOT_HOST_ID = "m3u8quicker-root-host";
  const APP_DEEP_LINK_BASE_URL = "m3u8quicker://new-task";
  const APP_BATCH_DEEP_LINK_BASE_URL = "m3u8quicker://batch-download";
  const APP_PREVIEW_DEEP_LINK_BASE_URL = "m3u8quicker://preview";
  const APP_LIVE_DEEP_LINK_BASE_URL = "m3u8quicker://new-live-record";
  const CHECK_PATTERN = /(png|image|ts|jpg|mp4|jpeg|EXTINF|MPD|SegmentTemplate|Representation)/i;
  const M3U8_PATTERN = /\.m3u8(?:$|[?#])/i;
  const DASH_PATTERN = /\.mpd(?:$|[?#])/i;
  const DIRECT_VIDEO_PATTERN = /\.(mp4|mkv|avi|wmv|flv|webm|mov|rmvb)(?:$|[?#])/i;
  const FLV_LIVE_PATTERN = /\.flv(?:$|[?#])/i;
  const BUTTON_LABEL = "M3U8 Quicker";
  const BUTTON_ICON_URL = chrome.runtime.getURL("icon.png");
  const MAX_DETECTED_TARGETS = 100;
  const isTopLevelContext = checkIsTopLevelContext();
  const detectedTargets = [];
  const checkedTargets = new Set();
  const videoThumbnailMap = new Map();
  let buttonPosition = { top: 20, right: 20 };
  let uiRoot = null;

  bindDetectionListener();
  injectSiteHooks();
  bindRuntimeListener();
  syncPendingDetections();
  waitForDomReady(() => {
    scanVideos();
    window.setInterval(scanVideos, 3000);
  });

  function injectSiteHooks() {
    if (!isTopLevelContext) {
      return;
    }
    const registry = Array.isArray(globalThis.__m3u8quickerInjects)
      ? globalThis.__m3u8quickerInjects
      : [];
    const hostname = window.location.hostname;
    registry.forEach((entry) => {
      if (!entry || !entry.hostPattern || !entry.script || !entry.flag) {
        return;
      }
      if (!entry.hostPattern.test(hostname)) {
        return;
      }
      if (document.documentElement.dataset[entry.flag] === "true") {
        return;
      }
      const script = document.createElement("script");
      script.src = chrome.runtime.getURL(entry.script);
      script.async = false;
      script.onload = () => script.remove();
      (document.head || document.documentElement).appendChild(script);
      document.documentElement.dataset[entry.flag] = "true";
    });
  }

  function bindDetectionListener() {
    window.addEventListener(DETECT_EVENT, (event) => {
      const url = event && event.detail ? event.detail.url : "";
      reportDetection(url);
    });
    window.addEventListener(CUSTOM_MANIFEST_EVENT, (event) => {
      const detail = event && event.detail ? event.detail : {};
      registerCustomManifest({
        source: detail.source,
        manifestJson: detail.manifest,
        title: detail.title,
      });
    });
    window.addEventListener(CUSTOM_TARGET_EVENT, (event) => {
      const detail = event && event.detail ? event.detail : {};
      registerCustomTarget(detail);
    });
  }

  function bindRuntimeListener() {
    chrome.runtime.onMessage.addListener((message) => {
      if (!isTopLevelContext || !message || message.type !== BACKGROUND_DETECT_MESSAGE) {
        return;
      }

      handleDetectedUrl(message.url);
    });
  }

  function waitForDomReady(callback) {
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", callback, { once: true });
      return;
    }
    callback();
  }

  function scanVideos() {
    const videos = document.getElementsByTagName("video");
    for (let i = 0; i < videos.length; i += 1) {
      const video = videos[i];
      const currentSrc = video.currentSrc || video.src || "";
      if (!isSupportedMediaUrl(currentSrc) || checkedTargets.has(currentSrc)) {
        continue;
      }

      const thumbnail = collectVideoThumbnail(video);
      if (thumbnail) {
        videoThumbnailMap.set(currentSrc, thumbnail);
      }
      reportDetection(currentSrc);
    }
    backfillThumbnails();
  }

  function collectVideoThumbnail(video) {
    if (!video) {
      return null;
    }
    if (video.poster) {
      return video.poster;
    }
    const nearby = findNearbyCoverImage(video);
    if (nearby) {
      return nearby;
    }
    return captureVideoFrame(video);
  }

  function findNearbyCoverImage(video) {
    const seen = new Set();
    let node = video.parentElement;
    for (let depth = 0; depth < 4 && node; depth += 1) {
      const imgs = node.querySelectorAll ? node.querySelectorAll("img") : [];
      let best = null;
      let bestArea = 0;
      for (let i = 0; i < imgs.length; i += 1) {
        const img = imgs[i];
        if (seen.has(img)) {
          continue;
        }
        seen.add(img);
        const src = img.currentSrc || img.src || "";
        if (!src) {
          continue;
        }
        const area = (img.naturalWidth || 0) * (img.naturalHeight || 0);
        if (area < 64 * 36) {
          continue;
        }
        if (area > bestArea) {
          best = src;
          bestArea = area;
        }
      }
      if (best) {
        return best;
      }
      node = node.parentElement;
    }
    return null;
  }

  function captureVideoFrame(video) {
    if (!video || video.readyState < 2 || !video.videoWidth) {
      return null;
    }
    try {
      const maxWidth = 240;
      const scale = Math.min(1, maxWidth / video.videoWidth);
      const width = Math.max(1, Math.round(video.videoWidth * scale));
      const height = Math.max(1, Math.round(video.videoHeight * scale));
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const ctx = canvas.getContext("2d");
      if (!ctx) {
        return null;
      }
      ctx.drawImage(video, 0, 0, width, height);
      return canvas.toDataURL("image/jpeg", 0.7);
    } catch (error) {
      return null;
    }
  }

  function resolveThumbnailFor(rawUrl) {
    if (!rawUrl) {
      return null;
    }
    if (videoThumbnailMap.has(rawUrl)) {
      return videoThumbnailMap.get(rawUrl);
    }
    const stripped = rawUrl.split("?")[0];
    if (stripped && videoThumbnailMap.has(stripped)) {
      return videoThumbnailMap.get(stripped);
    }
    for (const [key, value] of videoThumbnailMap) {
      if (key && key.split("?")[0] === stripped) {
        return value;
      }
    }
    return null;
  }

  function backfillThumbnails() {
    let changed = false;
    for (let i = 0; i < detectedTargets.length; i += 1) {
      const target = detectedTargets[i];
      if (target.thumbnail) {
        continue;
      }
      const resolved = resolveThumbnailFor(target.url) || resolveThumbnailFor(stripTitleParam(target.url));
      if (resolved) {
        target.thumbnail = resolved;
        changed = true;
      }
    }
    if (changed) {
      refreshOpenPanel();
    }
  }

  function stripTitleParam(url) {
    try {
      const urlObj = new URL(url, window.location.href);
      urlObj.searchParams.delete("title");
      return urlObj.href;
    } catch (error) {
      return url;
    }
  }

  function reportDetection(url) {
    if (!url) {
      return;
    }

    if (isTopLevelContext) {
      handleDetectedUrl(url);
      return;
    }

    try {
      chrome.runtime.sendMessage(
        {
          type: FRAME_DETECT_MESSAGE,
          url
        },
        () => {
          void chrome.runtime.lastError;
        }
      );
    } catch (error) {
      console.debug("[m3u8quicker] failed to relay frame detection", url, error);
    }
  }

  function syncPendingDetections() {
    if (!isTopLevelContext) {
      return;
    }

    try {
      chrome.runtime.sendMessage(
        {
          type: SYNC_DETECTIONS_MESSAGE
        },
        (response) => {
          if (chrome.runtime.lastError || !response || !Array.isArray(response.urls)) {
            return;
          }

          response.urls.forEach((url) => {
            handleDetectedUrl(url);
          });
        }
      );
    } catch (error) {
      console.debug("[m3u8quicker] failed to sync pending detections", error);
    }
  }

  function handleDetectedUrl(url) {
    if (!isTopLevelContext || !url || checkedTargets.has(url)) {
      return;
    }

    if (isFlvLiveUrl(url)) {
      checkedTargets.add(url);
      registerTarget(url, appendTitle(url));
      return;
    }

    if (isDirectVideoUrl(url)) {
      checkedTargets.add(url);
      registerTarget(url, appendTitle(url));
      return;
    }

    if (isM3u8Url(url) || isDashUrl(url)) {
      checkM3u8Url(url);
    }
  }

  async function checkM3u8Url(url) {
    if (!isTopLevelContext || !url || checkedTargets.has(url)) {
      return;
    }
    checkedTargets.add(url);

    const normalizedUrl = appendTitle(url);

    try {
      const response = await fetch(url, {
        method: "GET",
        credentials: "include"
      });

      if (!response.ok) {
        return;
      }

      const text = await response.text();
      if (!CHECK_PATTERN.test(text)) {
        return;
      }

      registerTarget(url, normalizedUrl);
    } catch (error) {
      console.debug("[m3u8quicker] failed to validate m3u8 url", url, error);
      registerTarget(url, normalizedUrl);
    }
  }

  function isSupportedMediaUrl(url) {
    return isM3u8Url(url) || isDashUrl(url) || isDirectVideoUrl(url);
  }

  function isM3u8Url(url) {
    return typeof url === "string" && M3U8_PATTERN.test(url);
  }

  function isDashUrl(url) {
    return typeof url === "string" && DASH_PATTERN.test(url);
  }

  function isDirectVideoUrl(url) {
    return typeof url === "string" && DIRECT_VIDEO_PATTERN.test(url);
  }

  function isFlvLiveUrl(url) {
    return typeof url === "string" && FLV_LIVE_PATTERN.test(url);
  }


  function detectFileType(url) {
    var directPattern = /\.(mp4|mkv|avi|wmv|flv|webm|mov|rmvb)$/i;
    var directPatternLoose = /\.(mp4|mkv|avi|wmv|flv|webm|mov|rmvb)(?:$|[?#])/i;
    try {
      var pathname = new URL(url, window.location.href).pathname;
      if (/\.mpd$/i.test(pathname)) return "dash";
      return directPattern.test(pathname) ? "mp4" : "hls";
    } catch (error) {
      if (/\.mpd(?:$|[?#])/i.test(url)) return "dash";
      return directPatternLoose.test(url) ? "mp4" : "hls";
    }
  }

  function detectFileExt(url) {
    var match = url.match(/\.(mp4|mkv|avi|wmv|flv|webm|mov|rmvb)(?:$|[?#])/i);
    return match ? match[1].toLowerCase() : "mp4";
  }

  function registerTarget(rawUrl, normalizedUrl) {
    let changed = false;
    if (!detectedTargets.find((item) => item.url === normalizedUrl)) {
      var type = detectFileType(rawUrl);
      var ext = detectFileExt(rawUrl);
      var fallback = type === "mp4"
        ? `video-${detectedTargets.length + 1}.${ext}`
        : type === "dash"
          ? `dash-${detectedTargets.length + 1}.mpd`
        : `m3u8-${detectedTargets.length + 1}.m3u8`;
      addDetectedTarget({
        url: normalizedUrl,
        fileName: getFileName(rawUrl, fallback),
        fileType: type,
        thumbnail: resolveThumbnailFor(rawUrl) || null,
        isLive: isFlvLiveUrl(rawUrl)
      });
      changed = true;
    } else {
      const existing = detectedTargets.find((item) => item.url === normalizedUrl);
      if (existing && !existing.thumbnail) {
        existing.thumbnail = resolveThumbnailFor(rawUrl) || null;
        changed = Boolean(existing.thumbnail);
      }
    }

    appendButton();
    updateButtonVisibility(true);
    if (changed) {
      refreshOpenPanel();
    }
  }

  function registerCustomTarget(detail) {
    if (!isTopLevelContext) {
      return;
    }
    const url = typeof detail.url === "string" ? detail.url.trim() : "";
    if (!url) {
      return;
    }
    const fileType = detail.fileType === "dash" || detail.fileType === "hls" ? detail.fileType : "mp4";
    const ext = fileType === "hls" ? "m3u8" : fileType === "dash" ? "mpd" : "mp4";
    const fallback = `${detail.source || "video"}-${detectedTargets.length + 1}.${ext}`;
    const fileName = sanitizeFilename(detail.fileName || "", fallback);
    if (detectedTargets.find((item) => item.url === url)) {
      return;
    }
    checkedTargets.add(url);
    addDetectedTarget({
      url,
      fileName: /\.[a-z0-9]{2,5}$/i.test(fileName) ? fileName : `${fileName}.${ext}`,
      fileType,
      thumbnail: detail.thumbnail || null,
    });
    appendButton();
    updateButtonVisibility(true);
    refreshOpenPanel();
  }

  function registerCustomManifest({ source, manifestJson, title }) {
    if (!isTopLevelContext) {
      return;
    }
    if (!manifestJson || typeof manifestJson !== "string") {
      return;
    }
    if (detectedTargets.find((item) => item.url === manifestJson)) {
      return;
    }
    const fallback = source ? `${source}-dash` : "custom-dash";
    const name = sanitizeFilename(title || getPageTitle(), fallback);
    addDetectedTarget({
      url: manifestJson,
      fileName: `${name}.json`,
      fileType: "dash",
      thumbnail: null,
    });
    checkedTargets.add(manifestJson);
    appendButton();
    updateButtonVisibility(true);
    refreshOpenPanel();
  }

  function addDetectedTarget(target) {
    detectedTargets.push(target);
    if (detectedTargets.length > MAX_DETECTED_TARGETS) {
      detectedTargets.splice(0, detectedTargets.length - MAX_DETECTED_TARGETS);
    }
  }

  function appendTitle(url) {
    try {
      const urlObj = new URL(url, window.location.href);
      urlObj.searchParams.set("title", getPageTitle());
      return urlObj.href;
    } catch (error) {
      return url;
    }
  }

  function getPageTitle() {
    let title = document.title;
    try {
      title = window.top.document.title || title;
    } catch (error) {
      console.debug("[m3u8quicker] failed to read top window title", error);
    }
    return title;
  }

  function getFileName(url, fallback) {
    const cleanUrl = String(url || "");
    const name = cleanUrl.slice(cleanUrl.lastIndexOf("/") + 1).split("?")[0];
    return name || fallback || "video.m3u8";
  }

  function sanitizeFilename(name, fallback) {
    const cleaned = String(name || "")
      .replace(/[<>:"/\\|?*\u0000-\u001f]/g, "_")
      .replace(/^\.+|\.+$/g, "")
      .trim();
    return cleaned || fallback;
  }

  function appendButton() {
    if (getUiElement(BUTTON_ID)) {
      return;
    }

    const button = document.createElement("button");
    button.id = BUTTON_ID;
    button.type = "button";
    button.innerHTML = `${getButtonIconMarkup()}<span>${BUTTON_LABEL}</span>`;
    button.style.position = "fixed";
    button.style.top = `${buttonPosition.top}px`;
    button.style.right = `${buttonPosition.right}px`;
    button.style.zIndex = "2147483647";
    button.style.display = "none";
    button.style.alignItems = "center";
    button.style.gap = "8px";
    button.style.padding = "10px 13px";
    button.style.border = "1px solid rgba(255,255,255,0.42)";
    button.style.borderRadius = "999px";
    button.style.background = "linear-gradient(135deg, rgba(9, 58, 130, 0.96), rgba(15, 108, 182, 0.96))";
    button.style.boxShadow = "0 14px 38px rgba(7, 31, 61, 0.28)";
    button.style.backdropFilter = "blur(8px)";
    button.style.color = "#ffffff";
    button.style.fontSize = "14px";
    button.style.lineHeight = "1.2";
    button.style.fontWeight = "600";
    button.style.cursor = "pointer";
    button.style.whiteSpace = "nowrap";
    button.style.width = "auto";
    button.style.maxWidth = "calc(100vw - 24px)";
    button.style.userSelect = "none";
    button.style.webkitFontSmoothing = "antialiased";
    button.style.transform = "translateZ(0)";
    button.style.fontFamily = "\"Segoe UI\", \"PingFang SC\", sans-serif";
    button.style.transition = "transform 140ms ease, box-shadow 140ms ease, opacity 140ms ease";
    button.style.opacity = "0.98";
    button.style.pointerEvents = "auto";
    enableButtonDrag(button);
    button.addEventListener("mouseenter", () => {
      button.style.transform = "translateY(-1px)";
      button.style.boxShadow = "0 18px 44px rgba(7, 31, 61, 0.34)";
    });
    button.addEventListener("mouseleave", () => {
      button.style.transform = "translateY(0)";
      button.style.boxShadow = "0 14px 38px rgba(7, 31, 61, 0.28)";
    });
    button.addEventListener("click", onButtonClick);

    getUiRoot().appendChild(button);
  }

  function updateButtonVisibility(visible) {
    const button = getUiElement(BUTTON_ID);
    if (!button) {
      return;
    }
    button.style.display = visible ? "inline-flex" : "none";
  }

  function clearDetectedTargets() {
    detectedTargets.splice(0, detectedTargets.length);
    const panel = getUiElement(PANEL_ID);
    if (panel) {
      panel.remove();
    }
  }

  function refreshOpenPanel() {
    const panel = getUiElement(PANEL_ID);
    if (!panel) {
      return;
    }
    panel.remove();
    onButtonClick();
  }

  function onButtonClick() {
    let panel = getUiElement(PANEL_ID);
    if (panel) {
      panel.remove();
      return;
    }

    panel = document.createElement("section");
    panel.id = PANEL_ID;
    panel.style.position = "fixed";
    panel.style.right = `${buttonPosition.right}px`;
    panel.style.top = `${buttonPosition.top + 52}px`;
    panel.style.zIndex = "2147483647";
    panel.style.width = "400px";
    panel.style.maxWidth = "min(520px, calc(100vw - 24px))";
    panel.style.maxHeight = "60vh";
    panel.style.overflowY = "auto";
    panel.style.padding = "12px";
    panel.style.borderRadius = "16px";
    panel.style.background = "rgba(255,255,255,0.98)";
    panel.style.border = "1px solid rgba(17, 85, 204, 0.16)";
    panel.style.boxShadow = "0 18px 45px rgba(15, 33, 62, 0.18)";
    panel.style.pointerEvents = "auto";

    const selectedUrls = new Set();
    const panelItems = buildTargetsWithUniqueNames(detectedTargets);

    const header = document.createElement("div");
    header.style.display = "flex";
    header.style.alignItems = "center";
    header.style.justifyContent = "space-between";
    header.style.gap = "8px";
    header.style.marginBottom = "10px";

    const title = document.createElement("div");
    title.textContent = "选择要下载的视频";
    title.style.color = "#17324d";
    title.style.fontSize = "13px";
    title.style.fontWeight = "700";
    title.style.flex = "1";

    const headerActions = document.createElement("div");
    headerActions.style.display = "flex";
    headerActions.style.alignItems = "center";
    headerActions.style.gap = "6px";
    const selectionInputs = [];

    const createTextActionButton = (text) => {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = text;
      button.style.border = "none";
      button.style.background = "transparent";
      button.style.color = "#5b718b";
      button.style.fontSize = "12px";
      button.style.cursor = "pointer";
      button.style.padding = "2px 4px";
      return button;
    };

    const selectAllButton = createTextActionButton("全选");
    const clearSelectionButton = createTextActionButton("不选");
    const clearListButton = createTextActionButton("清空");

    const batchButton = document.createElement("button");
    batchButton.type = "button";
    batchButton.textContent = "批量下载";
    batchButton.style.border = "1px solid rgba(17, 85, 204, 0.18)";
    batchButton.style.background = "#f2f7ff";
    batchButton.style.color = "#1155cc";
    batchButton.style.fontSize = "12px";
    batchButton.style.fontWeight = "600";
    batchButton.style.cursor = "pointer";
    batchButton.style.padding = "4px 8px";
    batchButton.style.borderRadius = "999px";

    const closeButton = document.createElement("button");
    closeButton.type = "button";
    closeButton.textContent = "关闭";
    closeButton.style.border = "none";
    closeButton.style.background = "transparent";
    closeButton.style.color = "#5b718b";
    closeButton.style.fontSize = "12px";
    closeButton.style.cursor = "pointer";
    closeButton.style.padding = "2px 4px";
    closeButton.addEventListener("click", () => {
      panel.remove();
    });

    const updateBatchButtonState = () => {
      const checkedCount = selectedUrls.size;
      batchButton.textContent = checkedCount > 0 ? `批量下载 (${checkedCount})` : "批量下载";
      batchButton.disabled = checkedCount === 0;
      batchButton.style.opacity = checkedCount > 0 ? "1" : "0.45";
      batchButton.style.cursor = checkedCount > 0 ? "pointer" : "not-allowed";
    };

    selectAllButton.addEventListener("click", () => {
      detectedTargets.forEach((item) => {
        selectedUrls.add(item.url);
      });
      selectionInputs.forEach((input) => {
        input.checked = true;
      });
      updateBatchButtonState();
    });

    clearSelectionButton.addEventListener("click", () => {
      selectedUrls.clear();
      selectionInputs.forEach((input) => {
        input.checked = false;
      });
      updateBatchButtonState();
    });

    clearListButton.addEventListener("click", () => {
      clearDetectedTargets();
    });

    batchButton.addEventListener("click", () => {
      if (selectedUrls.size === 0) {
        return;
      }

      openBatchDownloader(
        panelItems.filter((item) => selectedUrls.has(item.url))
      );
      panel.remove();
    });

    header.appendChild(title);
    headerActions.appendChild(selectAllButton);
    headerActions.appendChild(clearSelectionButton);
    headerActions.appendChild(clearListButton);
    headerActions.appendChild(batchButton);
    headerActions.appendChild(closeButton);
    header.appendChild(headerActions);
    panel.appendChild(header);

    if (panelItems.length === 0) {
      const empty = document.createElement("div");
      empty.textContent = "暂无可下载视频";
      empty.style.marginTop = "6px";
      empty.style.padding = "18px 12px";
      empty.style.border = "1px dashed #d8e2f1";
      empty.style.borderRadius = "10px";
      empty.style.background = "#f7fbff";
      empty.style.color = "#5b718b";
      empty.style.fontSize = "13px";
      empty.style.textAlign = "center";
      panel.appendChild(empty);
    }

    panelItems.forEach((item) => {
      const entry = document.createElement("div");
      entry.style.display = "flex";
      entry.style.alignItems = "flex-start";
      entry.style.gap = "8px";
      entry.style.marginTop = "6px";
      entry.style.padding = "10px 12px";
      entry.style.border = "1px solid #d8e2f1";
      entry.style.borderRadius = "10px";
      entry.style.background = "#f7fbff";

      const checkbox = document.createElement("input");
      checkbox.type = "checkbox";
      checkbox.checked = false;
      checkbox.style.margin = "3px 0 0";
      checkbox.style.cursor = "pointer";
      selectionInputs.push(checkbox);
      checkbox.addEventListener("change", () => {
        if (checkbox.checked) {
          selectedUrls.add(item.url);
        } else {
          selectedUrls.delete(item.url);
        }
        updateBatchButtonState();
      });

      const thumb = document.createElement("div");
      thumb.style.flex = "0 0 52px";
      thumb.style.width = "52px";
      thumb.style.height = "30px";
      thumb.style.borderRadius = "4px";
      thumb.style.overflow = "hidden";
      thumb.style.background = "#e8eef8";
      thumb.style.display = "flex";
      thumb.style.alignItems = "center";
      thumb.style.justifyContent = "center";

      const renderPlaceholder = () => {
        thumb.textContent = "";
        const placeholder = document.createElement("img");
        placeholder.src = BUTTON_ICON_URL;
        placeholder.alt = "";
        placeholder.style.width = "16px";
        placeholder.style.height = "16px";
        placeholder.style.opacity = "0.5";
        placeholder.style.objectFit = "contain";
        thumb.appendChild(placeholder);
      };

      if (item.thumbnail) {
        const img = document.createElement("img");
        img.src = item.thumbnail;
        img.alt = "";
        img.referrerPolicy = "no-referrer";
        img.style.width = "100%";
        img.style.height = "100%";
        img.style.objectFit = "cover";
        img.addEventListener("error", renderPlaceholder);
        thumb.appendChild(img);
      } else {
        renderPlaceholder();
      }

      const content = document.createElement("button");
      content.type = "button";
      content.title = isInlineDashJsonTarget(item) ? item.displayName : item.url;
      content.style.flex = "1";
      content.style.border = "none";
      content.style.padding = "0";
      content.style.background = "transparent";
      content.style.color = "#17324d";
      content.style.textAlign = "left";
      content.style.lineHeight = "1.45";
      content.style.whiteSpace = "normal";
      content.style.wordBreak = "break-word";
      content.style.overflowWrap = "anywhere";
      content.style.cursor = "pointer";
      content.addEventListener("click", () => {
        openDownloader(item.url, item.fileType || "hls");
        panel.remove();
      });

      const name = document.createElement("div");
      name.textContent = item.displayName;
      name.style.fontSize = "13px";
      name.style.fontWeight = "600";
      name.style.color = "#17324d";

      content.appendChild(name);

      const previewButton = document.createElement("button");
      previewButton.type = "button";
      previewButton.textContent = "预览";
      previewButton.style.border = "none";
      previewButton.style.background = "transparent";
      previewButton.style.color = "#1155cc";
      previewButton.style.fontSize = "12px";
      previewButton.style.cursor = "pointer";
      previewButton.style.padding = "2px 4px";
      previewButton.style.flex = "0 0 auto";
      previewButton.addEventListener("click", (event) => {
        event.stopPropagation();
        openPreview(item.url);
        panel.remove();
      });

      const copyButton = document.createElement("button");
      copyButton.type = "button";
      copyButton.textContent = "复制";
      copyButton.style.border = "none";
      copyButton.style.background = "transparent";
      copyButton.style.color = "#1155cc";
      copyButton.style.fontSize = "12px";
      copyButton.style.cursor = "pointer";
      copyButton.style.padding = "2px 4px";
      copyButton.style.flex = "0 0 auto";
      copyButton.addEventListener("click", async (event) => {
        event.stopPropagation();
        const copied = await copyTextToClipboard(item.url);
        const originalText = copyButton.textContent;
        copyButton.textContent = copied ? "已复制" : "失败";
        window.setTimeout(() => {
          copyButton.textContent = originalText;
        }, 1200);
      });

      const downloadButton = document.createElement("button");
      downloadButton.type = "button";
      downloadButton.textContent = "下载";
      downloadButton.style.border = "none";
      downloadButton.style.background = "transparent";
      downloadButton.style.color = "#1155cc";
      downloadButton.style.fontSize = "12px";
      downloadButton.style.cursor = "pointer";
      downloadButton.style.padding = "2px 4px";
      downloadButton.style.flex = "0 0 auto";
      downloadButton.addEventListener("click", (event) => {
        event.stopPropagation();
        openDownloader(item.url, item.fileType || "hls");
        panel.remove();
      });

      const actions = document.createElement("div");
      actions.style.display = "flex";
      actions.style.alignItems = "center";
      actions.style.gap = "2px";
      actions.style.flex = "0 0 auto";
      actions.appendChild(copyButton);
      actions.appendChild(previewButton);
      actions.appendChild(downloadButton);

      if (item.isLive) {
        const recordButton = document.createElement("button");
        recordButton.type = "button";
        recordButton.textContent = "录制";
        recordButton.style.border = "none";
        recordButton.style.background = "transparent";
        recordButton.style.color = "#1155cc";
        recordButton.style.fontSize = "12px";
        recordButton.style.cursor = "pointer";
        recordButton.style.padding = "2px 4px";
        recordButton.style.flex = "0 0 auto";
        recordButton.addEventListener("click", (event) => {
          event.stopPropagation();
          openLiveRecorder(item.url);
          panel.remove();
        });
        actions.appendChild(recordButton);
      }

      entry.appendChild(checkbox);
      entry.appendChild(thumb);
      entry.appendChild(content);
      entry.appendChild(actions);
      panel.appendChild(entry);
    });

    updateBatchButtonState();
    getUiRoot().appendChild(panel);
  }

  function openDownloader(target, fileType) {
    if (!target) {
      return;
    }
    const params = new URLSearchParams({
      url: target,
    });
    if (fileType && fileType !== "hls") {
      params.set("file_type", fileType);
    }
    const extraHeaders = buildExtraHeaders();
    if (extraHeaders) {
      params.set("extra_headers", extraHeaders);
    }

    const url = `${APP_DEEP_LINK_BASE_URL}?${params.toString()}`;
    window.location.href = url;
  }

  function isInlineDashJsonTarget(item) {
    return item && item.fileType === "dash" && typeof item.url === "string" && item.url.trim().startsWith("{");
  }

  function openPreview(target) {
    if (!target) {
      return;
    }
    const params = new URLSearchParams({
      url: target,
    });
    const extraHeaders = buildExtraHeaders();
    if (extraHeaders) {
      params.set("extra_headers", extraHeaders);
    }
    window.location.href = `${APP_PREVIEW_DEEP_LINK_BASE_URL}?${params.toString()}`;
  }

  function openLiveRecorder(target) {
    if (!target) {
      return;
    }
    const params = new URLSearchParams({
      url: target,
    });
    const extraHeaders = buildExtraHeaders();
    if (extraHeaders) {
      params.set("extra_headers", extraHeaders);
    }
    window.location.href = `${APP_LIVE_DEEP_LINK_BASE_URL}?${params.toString()}`;
  }

  function openBatchDownloader(items) {
    if (!items || items.length === 0) {
      return;
    }

    const params = new URLSearchParams({
      items: items.map((item) => item.batchUrl || item.url).join("\n")
    });
    const extraHeaders = buildExtraHeaders();
    if (extraHeaders) {
      params.set("extra_headers", extraHeaders);
    }

    window.location.href = `${APP_BATCH_DEEP_LINK_BASE_URL}?${params.toString()}`;
  }

  function buildTargetsWithUniqueNames(items) {
    const totals = new Map();
    items.forEach((item) => {
      const key = String(getCurrentTitleFromUrl(item.url) || "").trim().toLowerCase();
      if (!key) {
        return;
      }
      totals.set(key, (totals.get(key) || 0) + 1);
    });

    const indexes = new Map();
    return items.map((item) => {
      const title = String(getCurrentTitleFromUrl(item.url) || "").trim();
      const key = title.toLowerCase();
      const total = totals.get(key) || 0;
      if (!key || total <= 1) {
        return {
          ...item,
          displayName: item.fileName || "video",
          batchUrl: item.url
        };
      }

      const nextIndex = (indexes.get(key) || 0) + 1;
      indexes.set(key, nextIndex);
      return {
        ...item,
        displayName: item.fileName || "video",
        batchUrl: appendCustomTitle(item.url, `${title}-${nextIndex}`)
      };
    });
  }

  function getCurrentTitleFromUrl(url) {
    try {
      const urlObj = new URL(url, window.location.href);
      return urlObj.searchParams.get("title") || "";
    } catch (error) {
      return "";
    }
  }

  function appendCustomTitle(url, title) {
    try {
      const urlObj = new URL(url, window.location.href);
      urlObj.searchParams.set("title", title);
      return urlObj.href;
    } catch (error) {
      return url;
    }
  }

  async function copyTextToClipboard(text) {
    try {
      if (navigator.clipboard && navigator.clipboard.writeText) {
        await navigator.clipboard.writeText(text);
        return true;
      }
    } catch (error) {
      console.debug("[m3u8quicker] clipboard api failed", error);
    }

    try {
      const input = document.createElement("textarea");
      input.value = text;
      input.setAttribute("readonly", "readonly");
      input.style.position = "fixed";
      input.style.top = "-9999px";
      document.body.appendChild(input);
      input.select();
      const copied = document.execCommand("copy");
      document.body.removeChild(input);
      return copied;
    } catch (error) {
      console.debug("[m3u8quicker] execCommand copy failed", error);
      return false;
    }
  }

  function buildExtraHeaders() {
    const headers = [];
    const pageUrl = window.location.href;
    if (pageUrl) {
      headers.push(`referer:${pageUrl}`);
    }

    try {
      headers.push(`origin:${window.location.origin}`);
    } catch (error) {
      console.debug("[m3u8quicker] failed to read origin", error);
    }

    if (navigator.userAgent) {
      headers.push(`user-agent:${navigator.userAgent}`);
    }

    return headers.filter(Boolean).join("\n");
  }

  function getButtonIconMarkup() {
    return [
      '<span style="display:inline-flex;align-items:center;justify-content:center;width:18px;height:18px;border-radius:999px;background:rgba(255,255,255,0.14);padding:1px;box-sizing:border-box;flex:0 0 18px;">',
      `<img src="${BUTTON_ICON_URL}" alt="" style="display:block;width:100%;height:100%;border-radius:999px;object-fit:cover;" />`,
      "</span>",
    ].join("");
  }

  function enableButtonDrag(button) {
    const dragState = {
      active: false,
      moved: false,
      startX: 0,
      startY: 0,
      pointerId: null,
      rectLeft: 0,
      rectTop: 0,
    };

    button.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) {
        return;
      }

      const rect = button.getBoundingClientRect();
      dragState.active = true;
      dragState.moved = false;
      dragState.startX = event.clientX;
      dragState.startY = event.clientY;
      dragState.pointerId = event.pointerId;
      dragState.rectLeft = rect.left;
      dragState.rectTop = rect.top;
      button.setPointerCapture(event.pointerId);
      button.style.transition = "none";
    });

    button.addEventListener("pointermove", (event) => {
      if (!dragState.active || dragState.pointerId !== event.pointerId) {
        return;
      }

      const deltaX = event.clientX - dragState.startX;
      const deltaY = event.clientY - dragState.startY;
      if (!dragState.moved && Math.hypot(deltaX, deltaY) > 4) {
        dragState.moved = true;
      }
      if (!dragState.moved) {
        return;
      }

      const maxLeft = Math.max(0, window.innerWidth - button.offsetWidth);
      const maxTop = Math.max(0, window.innerHeight - button.offsetHeight);
      const nextLeft = clamp(dragState.rectLeft + deltaX, 0, maxLeft);
      const nextTop = clamp(dragState.rectTop + deltaY, 0, maxTop);

      buttonPosition = {
        top: nextTop,
        right: Math.max(0, window.innerWidth - nextLeft - button.offsetWidth),
      };
      button.style.top = `${buttonPosition.top}px`;
      button.style.right = `${buttonPosition.right}px`;

      const panel = getUiElement(PANEL_ID);
      if (panel) {
        panel.style.top = `${buttonPosition.top + 52}px`;
        panel.style.right = `${buttonPosition.right}px`;
      }
    });

    const finishDrag = (event) => {
      if (!dragState.active || dragState.pointerId !== event.pointerId) {
        return;
      }

      dragState.active = false;
      dragState.pointerId = null;
      button.style.transition = "transform 140ms ease, box-shadow 140ms ease, opacity 140ms ease";
      button.releasePointerCapture(event.pointerId);
    };

    button.addEventListener("pointerup", finishDrag);
    button.addEventListener("pointercancel", finishDrag);
    button.addEventListener("click", (event) => {
      if (dragState.moved) {
        event.preventDefault();
        event.stopPropagation();
        dragState.moved = false;
      }
    }, true);
  }

  function clamp(value, min, max) {
    return Math.min(Math.max(value, min), max);
  }

  function getUiElement(id) {
    return getUiRoot().querySelector(`#${id}`);
  }

  function getUiRoot() {
    if (uiRoot && uiRoot.isConnected) {
      return uiRoot;
    }

    let host = document.getElementById(ROOT_HOST_ID);
    if (!host) {
      host = document.createElement("div");
      host.id = ROOT_HOST_ID;
      host.style.setProperty("all", "initial", "important");
      host.style.setProperty("position", "fixed", "important");
      host.style.setProperty("inset", "0", "important");
      host.style.setProperty("display", "block", "important");
      host.style.setProperty("z-index", "2147483647", "important");
      host.style.setProperty("pointer-events", "none", "important");
      getMountNode().appendChild(host);
    }

    const shadowRoot = host.shadowRoot || host.attachShadow({ mode: "open" });
    uiRoot = shadowRoot.querySelector("[data-m3u8quicker-root]");
    if (!uiRoot) {
      uiRoot = document.createElement("div");
      uiRoot.dataset.m3u8quickerRoot = "true";
      uiRoot.style.position = "fixed";
      uiRoot.style.inset = "0";
      uiRoot.style.pointerEvents = "none";
      uiRoot.style.zIndex = "2147483647";
      shadowRoot.appendChild(uiRoot);
    }

    return uiRoot;
  }

  function getMountNode() {
    return document.body || document.documentElement;
  }

  function checkIsTopLevelContext() {
    try {
      return window.top === window;
    } catch (error) {
      return false;
    }
  }
})();
