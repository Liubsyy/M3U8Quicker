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
    const urls = video && video.play_addr && video.play_addr.url_list;
    if (!Array.isArray(urls) || urls.length === 0) return;
    const link =
      urls.find((u) => typeof u === "string" && !/\/aweme\/v1\//i.test(u)) ||
      urls[0];
    if (typeof link !== "string" || !link) return;
    emit(link, clean(item.desc) || "douyin-" + id, cover(video));
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

  function emit(rawUrl, title, thumbnail) {
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
          thumbnail: thumbnail
        }
      })
    );
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
