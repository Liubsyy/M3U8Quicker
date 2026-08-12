(function () {
  const TARGET_EVENT = "m3u8quicker:custom-target";
  const DETAIL_ENDPOINT = "https://www.douyin.com/aweme/v1/web/aweme/detail/";
  const handled = new Set();

  function tick() {
    const fromPath = location.pathname.match(/\/video\/(\d+)/);
    if (fromPath) submit(fromPath[1]);
    document
      .querySelectorAll(".video-info-detail[data-e2e-aweme-id]")
      .forEach((node) => submit(node.getAttribute("data-e2e-aweme-id")));
  }

  function submit(id) {
    if (!id || handled.has(id)) return;
    handled.add(id);
    load(id).catch((err) => {
      handled.delete(id);
      console.debug("[m3u8quicker] douyin lookup failed", id, err);
    });
  }

  async function load(id) {
    const res = await fetch(
      DETAIL_ENDPOINT + "?aweme_id=" + encodeURIComponent(id),
      { credentials: "include", headers: { Accept: "application/json" } }
    );
    if (!res.ok) {
      handled.delete(id);
      return;
    }
    const body = await res.json();
    const item = body && body.aweme_detail;
    const video = item && item.video;
    const qualities = collectQualities(video);
    if (qualities.length === 0) return;
    const title = clean(item.desc) || "douyin-" + id;
    emit(qualities[0].url, title, cover(video), qualities, id);
  }

  function collectQualities(video) {
    if (!video || typeof video !== "object") return [];

    const candidates = [];
    if (Array.isArray(video.bit_rate)) {
      video.bit_rate.forEach((rate) => {
        if (!rate || typeof rate !== "object") return;
        const address = rate.play_addr || rate.play_addr_265 || rate.play_addr_h264;
        const url = pickUrl(address);
        if (!url) return;
        const height = positiveNumber(address && address.height) || inferHeight(rate.gear_name);
        const width = positiveNumber(address && address.width);
        const bitrate = positiveNumber(rate.bit_rate);
        candidates.push({ url, height, width, bitrate });
      });
    }

    ["play_addr", "play_addr_h264", "play_addr_265", "play_addr_h265", "play_addr_bytevc1"]
      .forEach((key) => {
        const address = video[key];
        const url = pickUrl(address);
        if (!url) return;
        candidates.push({
          url,
          height: positiveNumber(address.height),
          width: positiveNumber(address.width),
          bitrate: positiveNumber(address.bit_rate)
        });
      });

    candidates.sort((a, b) =>
      (b.height || 0) - (a.height || 0) ||
      (b.bitrate || 0) - (a.bitrate || 0)
    );

    const seenUrls = new Set();
    const seenHeights = new Set();
    const results = [];
    candidates.forEach((candidate) => {
      if (seenUrls.has(candidate.url)) return;
      const heightKey = candidate.height ? String(candidate.height) : "";
      if (heightKey && seenHeights.has(heightKey)) return;
      seenUrls.add(candidate.url);
      if (heightKey) seenHeights.add(heightKey);
      results.push({
        url: candidate.url,
        label: qualityLabel(candidate, results.length),
        height: candidate.height || undefined,
        width: candidate.width || undefined,
        bitrate: candidate.bitrate || undefined
      });
    });
    return results;
  }

  function pickUrl(address) {
    const urls = address && Array.isArray(address.url_list) ? address.url_list : [];
    return urls.find((url) => typeof url === "string" && url && !/\/aweme\/v1\//i.test(url)) ||
      urls.find((url) => typeof url === "string" && url) || "";
  }

  function positiveNumber(value) {
    const number = Number(value);
    return Number.isFinite(number) && number > 0 ? number : 0;
  }

  function inferHeight(gearName) {
    const value = String(gearName || "").toLowerCase();
    if (/4k/.test(value)) return 2160;
    if (/2k/.test(value)) return 1440;
    const matches = value.match(/(?:^|_)(\d{3,4})(?:_|$)/g) || [];
    return matches.reduce((best, part) => {
      const value = Number(part.replace(/_/g, ""));
      return value >= 240 && value <= 2160 ? Math.max(best, value) : best;
    }, 0);
  }

  function qualityLabel(candidate, index) {
    const parts = [];
    if (candidate.width && candidate.height) {
      parts.push(candidate.width + "×" + candidate.height + "（" + candidate.height + "P）");
    } else if (candidate.height) {
      parts.push(candidate.height + "P");
    } else if (index === 0) {
      parts.push("默认清晰度");
    } else {
      parts.push("清晰度 " + (index + 1));
    }
    if (candidate.bitrate) {
      parts.push((candidate.bitrate / 1000000).toFixed(1) + " Mbps");
    }
    return parts.join(" · ");
  }

  function cover(video) {
    const lists = [video.cover, video.origin_cover, video.dynamic_cover];
    for (const c of lists) {
      const first = c && Array.isArray(c.url_list) ? c.url_list[0] : null;
      if (typeof first === "string" && first) return first;
    }
    return null;
  }

  function clean(text) {
    return String(text || "")
      .replace(/[<>:"/\\|?*]/g, "_")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 80);
  }

  function emit(rawUrl, title, thumbnail, qualities, id) {
    let url = rawUrl;
    try {
      const u = new URL(rawUrl);
      u.searchParams.set("title", title);
      url = u.href;
    } catch (e) {
      /* keep raw */
    }
    window.dispatchEvent(
      new CustomEvent(TARGET_EVENT, {
        detail: {
          source: "douyin",
          url: url,
          fileName: title + ".mp4",
          fileType: "mp4",
          thumbnail: thumbnail,
          groupId: "douyin:" + id,
          qualities: qualities.map((quality) => ({
            ...quality,
            url: withTitle(quality.url, title)
          }))
        }
      })
    );
  }

  function withTitle(rawUrl, title) {
    try {
      const url = new URL(rawUrl);
      url.searchParams.set("title", title);
      return url.href;
    } catch (e) {
      return rawUrl;
    }
  }

  function boot() {
    tick();
    setInterval(tick, 2000);
    new MutationObserver(tick).observe(document.documentElement, {
      childList: true,
      subtree: true
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot, { once: true });
  } else {
    boot();
  }
})();
