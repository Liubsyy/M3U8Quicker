(function () {
  const DETECT_EVENT = "m3u8quicker:detected";
  const ORIGINAL_XHR = window.XMLHttpRequest;
  const originalFetch = window.fetch;

  function notify(url) {
    if (!url || typeof url !== "string" || url.indexOf(".m3u8") === -1) {
      return;
    }

    window.dispatchEvent(
      new CustomEvent(DETECT_EVENT, {
        detail: { url }
      })
    );
  }

  if (window.__m3u8quickerNetworkHooked) {
    return;
  }
  window.__m3u8quickerNetworkHooked = true;

  const originOpen = ORIGINAL_XHR.prototype.open;
  window.XMLHttpRequest = function XMLHttpRequestProxy() {
    const realXHR = new ORIGINAL_XHR();

    realXHR.open = function patchedOpen(method, url) {
      try {
        notify(String(url || ""));
      } catch (error) {
        console.debug("[m3u8quicker] failed to inspect XHR url", error);
      }

      return originOpen.apply(realXHR, arguments);
    };

    return realXHR;
  };

  window.XMLHttpRequest.UNSENT = ORIGINAL_XHR.UNSENT;
  window.XMLHttpRequest.OPENED = ORIGINAL_XHR.OPENED;
  window.XMLHttpRequest.HEADERS_RECEIVED = ORIGINAL_XHR.HEADERS_RECEIVED;
  window.XMLHttpRequest.LOADING = ORIGINAL_XHR.LOADING;
  window.XMLHttpRequest.DONE = ORIGINAL_XHR.DONE;
  window.XMLHttpRequest.prototype = ORIGINAL_XHR.prototype;

  if (typeof originalFetch === "function") {
    window.fetch = function patchedFetch(resource, init) {
      try {
        if (typeof resource === "string") {
          notify(resource);
        } else if (resource && typeof resource.url === "string") {
          notify(resource.url);
        }
      } catch (error) {
        console.debug("[m3u8quicker] failed to inspect fetch url", error);
      }

      return originalFetch.call(this, resource, init);
    };
  }
})();
