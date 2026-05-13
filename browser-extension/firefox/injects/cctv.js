(function () {
  const TARGET_EVENT = "m3u8quicker:custom-target";
  const API_PATTERN = /\/\/vdn\.apps\.cntv\.cn\/api\/getHttpVideoInfo\.do(?:$|[?#])/i;
  const handledUrls = new Set();

  patchFetch();
  patchXhr();

  function isVideoInfoUrl(url) {
    return typeof url === "string" && API_PATTERN.test(url);
  }

  function getFetchUrl(input) {
    if (typeof input === "string") return input;
    if (input && typeof input.url === "string") return input.url;
    return "";
  }

  function patchFetch() {
    if (typeof window.fetch !== "function") {
      return;
    }
    const nativeFetch = window.fetch;
    window.fetch = function (...args) {
      const requestUrl = getFetchUrl(args[0]);
      return nativeFetch.apply(this, args).then((response) => {
        if (!isVideoInfoUrl(requestUrl)) {
          return response;
        }

        response.clone().json().then(handleVideoInfo).catch(() => {});
        return response;
      });
    };
  }

  function patchXhr() {
    if (typeof window.XMLHttpRequest !== "function") {
      return;
    }

    const nativeOpen = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (...args) {
      if (isVideoInfoUrl(args[1])) {
        this.addEventListener("load", () => {
          const body = parseXhrResponse(this);
          if (body) {
            handleVideoInfo(body);
          }
        });
      }
      return nativeOpen.apply(this, args);
    };
  }

  function parseXhrResponse(xhr) {
    try {
      if (xhr.responseType === "json" && xhr.response && typeof xhr.response === "object") {
        return xhr.response;
      }
      return xhr.responseText ? JSON.parse(xhr.responseText) : null;
    } catch (error) {
      return null;
    }
  }

  function handleVideoInfo(body) {
    if (!body || typeof body !== "object") {
      return;
    }

    const hlsUrl = typeof body.hls_url === "string" ? body.hls_url.trim() : "";
    if (!hlsUrl || handledUrls.has(hlsUrl)) {
      return;
    }
    handledUrls.add(hlsUrl);

    const title = cleanTitle(body.title) || cleanTitle(document.title) || "cctv-video";
    emit(hlsUrl, title);
  }

  function emit(rawUrl, title) {
    let url = rawUrl;
    try {
      const urlObj = new URL(rawUrl, window.location.href);
      urlObj.searchParams.set("title", title);
      url = urlObj.href;
    } catch (error) {
      // keep raw url
    }

    window.dispatchEvent(
      new CustomEvent(TARGET_EVENT, {
        detail: {
          source: "cctv",
          url,
          fileName: title + ".m3u8",
          fileType: "hls",
          thumbnail: null,
        },
      })
    );
  }

  function cleanTitle(text) {
    return String(text || "")
      .replace(/[<>:"/\\|?*\u0000-\u001f]/g, "_")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 100);
  }
})();