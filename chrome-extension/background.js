const BACKGROUND_DETECT_MESSAGE = "m3u8quicker:network-detected";
const FRAME_DETECT_MESSAGE = "m3u8quicker:frame-detected";
const SYNC_DETECTIONS_MESSAGE = "m3u8quicker:sync-detections";
const M3U8_PATTERN = /\.m3u8(?:$|[?#])/i;
const pendingDetectionsByTab = new Map();

chrome.webRequest.onBeforeRequest.addListener(
  (details) => {
    if (!shouldTrackRequest(details)) {
      return;
    }

    queueAndDispatch(details.tabId, details.url);
  },
  {
    urls: ["<all_urls>"]
  }
);

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (!message || typeof message !== "object") {
    return;
  }

  if (message.type === FRAME_DETECT_MESSAGE && sender.tab?.id >= 0) {
    queueAndDispatch(sender.tab.id, message.url);
    return;
  }

  if (message.type === SYNC_DETECTIONS_MESSAGE) {
    const tabId = sender.tab?.id ?? -1;
    sendResponse({
      urls: popPendingDetections(tabId)
    });
  }
});

function shouldTrackRequest(details) {
  return details.tabId >= 0 && typeof details.url === "string" && M3U8_PATTERN.test(details.url);
}

function queueAndDispatch(tabId, url) {
  if (tabId < 0 || typeof url !== "string" || !url) {
    return;
  }

  queueDetection(tabId, url);
  dispatchDetection(tabId, url);
}

function queueDetection(tabId, url) {
  const queued = pendingDetectionsByTab.get(tabId) ?? [];
  if (!queued.includes(url)) {
    queued.push(url);
    pendingDetectionsByTab.set(tabId, queued);
  }
}

function dispatchDetection(tabId, url) {
  chrome.tabs.sendMessage(
    tabId,
    {
      type: BACKGROUND_DETECT_MESSAGE,
      url
    },
    {
      frameId: 0
    },
    () => {
      if (chrome.runtime.lastError) {
        return;
      }

      removePendingDetection(tabId, url);
    }
  );
}

function popPendingDetections(tabId) {
  if (tabId < 0) {
    return [];
  }

  const queued = pendingDetectionsByTab.get(tabId) ?? [];
  pendingDetectionsByTab.delete(tabId);
  return queued;
}

function removePendingDetection(tabId, url) {
  const queued = pendingDetectionsByTab.get(tabId);
  if (!queued) {
    return;
  }

  const nextQueued = queued.filter((item) => item !== url);
  if (nextQueued.length === 0) {
    pendingDetectionsByTab.delete(tabId);
    return;
  }

  pendingDetectionsByTab.set(tabId, nextQueued);
}
