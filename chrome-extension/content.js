(() => {
  const DETECT_EVENT = "m3u8quicker:detected";
  const BACKGROUND_DETECT_MESSAGE = "m3u8quicker:network-detected";
  const FRAME_DETECT_MESSAGE = "m3u8quicker:frame-detected";
  const SYNC_DETECTIONS_MESSAGE = "m3u8quicker:sync-detections";
  const BUTTON_ID = "m3u8quicker-download-button";
  const PANEL_ID = "m3u8quicker-download-panel";
  const ROOT_HOST_ID = "m3u8quicker-root-host";
  const APP_DEEP_LINK_BASE_URL = "m3u8quicker://new-task";
  const CHECK_PATTERN = /(png|image|ts|jpg|mp4|jpeg|EXTINF)/i;
  const BUTTON_LABEL = "M3U8 Quicker";
  const BUTTON_ICON_URL = chrome.runtime.getURL("icon.png");
  const isTopLevelContext = checkIsTopLevelContext();
  const detectedTargets = [];
  const checkedTargets = new Set();
  let latestTarget = "";
  let buttonPosition = { top: 20, right: 20 };
  let uiRoot = null;

  injectNetworkHook();
  bindDetectionListener();
  bindRuntimeListener();
  syncPendingDetections();
  waitForDomReady(() => {
    scanVideos();
    window.setInterval(scanVideos, 3000);
  });

  function injectNetworkHook() {
    if (document.documentElement.dataset.m3u8quickerInjected === "true") {
      return;
    }

    const script = document.createElement("script");
    script.src = chrome.runtime.getURL("injected-network.js");
    script.async = false;
    script.onload = () => script.remove();
    (document.head || document.documentElement).appendChild(script);
    document.documentElement.dataset.m3u8quickerInjected = "true";
  }

  function bindDetectionListener() {
    window.addEventListener(DETECT_EVENT, (event) => {
      const url = event && event.detail ? event.detail.url : "";
      reportDetection(url);
    });
  }

  function bindRuntimeListener() {
    chrome.runtime.onMessage.addListener((message) => {
      if (!isTopLevelContext || !message || message.type !== BACKGROUND_DETECT_MESSAGE) {
        return;
      }

      checkM3u8Url(message.url);
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
      const currentSrc = videos[i].currentSrc || videos[i].src || "";
      if (currentSrc.indexOf(".m3u8") > -1) {
        reportDetection(currentSrc);
      }
    }
  }

  function reportDetection(url) {
    if (!url) {
      return;
    }

    if (isTopLevelContext) {
      checkM3u8Url(url);
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
            checkM3u8Url(url);
          });
        }
      );
    } catch (error) {
      console.debug("[m3u8quicker] failed to sync pending detections", error);
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

  function registerTarget(rawUrl, normalizedUrl) {
    latestTarget = normalizedUrl;
    if (!detectedTargets.find((item) => item.url === normalizedUrl)) {
      detectedTargets.push({
        url: normalizedUrl,
        fileName: getFileName(rawUrl, `m3u8-${detectedTargets.length + 1}.m3u8`)
      });
    }

    appendButton();
    updateButtonVisibility(true);
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

  function onButtonClick() {
    if (detectedTargets.length <= 1) {
      openDownloader(latestTarget);
      return;
    }

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
    panel.style.width = "320px";
    panel.style.maxWidth = "min(420px, calc(100vw - 24px))";
    panel.style.maxHeight = "50vh";
    panel.style.overflowY = "auto";
    panel.style.padding = "12px";
    panel.style.borderRadius = "16px";
    panel.style.background = "rgba(255,255,255,0.98)";
    panel.style.border = "1px solid rgba(17, 85, 204, 0.16)";
    panel.style.boxShadow = "0 18px 45px rgba(15, 33, 62, 0.18)";
    panel.style.pointerEvents = "auto";

    const header = document.createElement("div");
    header.style.display = "flex";
    header.style.alignItems = "center";
    header.style.justifyContent = "space-between";
    header.style.gap = "8px";
    header.style.marginBottom = "10px";

    const title = document.createElement("div");
    title.textContent = "选择要下载的 m3u8";
    title.style.color = "#17324d";
    title.style.fontSize = "13px";
    title.style.fontWeight = "700";
    title.style.flex = "1";

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

    header.appendChild(title);
    header.appendChild(closeButton);
    panel.appendChild(header);

    detectedTargets.forEach((item) => {
      const entry = document.createElement("button");
      entry.type = "button";
      entry.textContent = item.fileName;
      entry.title = item.url;
      entry.style.display = "block";
      entry.style.width = "100%";
      entry.style.marginTop = "6px";
      entry.style.padding = "10px 12px";
      entry.style.border = "1px solid #d8e2f1";
      entry.style.borderRadius = "10px";
      entry.style.background = "#f7fbff";
      entry.style.color = "#17324d";
      entry.style.textAlign = "left";
      entry.style.lineHeight = "1.45";
      entry.style.whiteSpace = "normal";
      entry.style.wordBreak = "break-word";
      entry.style.overflowWrap = "anywhere";
      entry.style.cursor = "pointer";
      entry.addEventListener("click", () => {
        openDownloader(item.url);
        panel.remove();
      });
      panel.appendChild(entry);
    });

    getUiRoot().appendChild(panel);
  }

  function openDownloader(target) {
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

    const url = `${APP_DEEP_LINK_BASE_URL}?${params.toString()}`;
    window.location.href = url;
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
