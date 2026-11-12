// Loaded in the extension background, where host permissions allow loopback requests.
(() => {
  const PORT_CACHE_KEY = "m3u8quickerBatchPort";
  let cachedPort = null;
  let cacheLoaded = null;
  let cacheWrite = Promise.resolve();
  const validPort = (port) => Number.isInteger(port) && port >= 49320 && port <= 49329;

  function storageCall(method, value) {
    if (typeof browser !== "undefined" && browser.storage) {
      return browser.storage.local[method](value);
    }
    return new Promise((resolve, reject) => {
      chrome.storage.local[method](value, (result) => {
        const error = chrome.runtime.lastError;
        if (error) reject(new Error(error.message));
        else resolve(result);
      });
    });
  }

  function loadPort() {
    cacheLoaded ??= (async () => {
      try {
        const stored = await storageCall("get", PORT_CACHE_KEY);
        cachedPort = validPort(stored?.[PORT_CACHE_KEY]) ? stored[PORT_CACHE_KEY] : null;
      } catch {
        // Storage being unavailable must not prevent a transfer.
      }
    })();
    return cacheLoaded;
  }

  function rememberPort(port) {
    if (cachedPort === port) return;
    cachedPort = port;
    // Keep writes ordered across concurrent tabs, without delaying the upload.
    cacheWrite = cacheWrite.then(() => storageCall("set", { [PORT_CACHE_KEY]: port })).catch(() => {});
  }

  function forgetPort(port) {
    if (cachedPort === port) rememberPort(null);
  }

  globalThis.m3u8quickerTransferBatchAttempt = async function ({ id, secret, payload }) {
    if (!/^[a-f0-9]{32}$/.test(id) || !/^[a-f0-9]{64}$/.test(secret)) {
      return { ok: false, error: "invalid" };
    }
    const body = JSON.stringify(payload);
    if (new TextEncoder().encode(body).length > 16 * 1024 * 1024) {
      return { ok: false, error: "tooLarge" };
    }
    const request = async (url, options = {}, timeout = 900, controller = new AbortController()) => {
      const timer = setTimeout(() => controller.abort(), timeout);
      try {
        const response = await fetch(url, { ...options, signal: controller.signal, cache: "no-store", credentials: "omit", redirect: "error" });
        const text = await response.text();
        return { status: response.status, ok: response.ok, json: () => JSON.parse(text) };
      } finally {
        clearTimeout(timer);
      }
    };
    const challenge = Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, "0")).join("");
    const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["verify"]);
    const probe = async (port, controller = new AbortController()) => {
      const base = `http://127.0.0.1:${port}/batch/v1/${id}`;
      try {
        const response = await request(`${base}/probe?challenge=${challenge}`, {}, 900, controller);
        if (!response.ok) return null;
        const result = await response.json();
        if (!/^[a-f0-9]{64}$/.test(result.proof) || !["ready", "delivered"].includes(result.status)) return null;
        const proof = Uint8Array.from(result.proof.match(/../g), (hex) => parseInt(hex, 16));
        const valid = await crypto.subtle.verify("HMAC", key, proof,
          new TextEncoder().encode(`m3u8quicker-batch-v1:${id}:${challenge}:${result.status}`));
        return valid && !controller.signal.aborted ? { base, port, status: result.status } : null;
      } catch {
        return null;
      }
    };
    await loadPort();
    const preferredPort = cachedPort;
    let target = preferredPort === null ? null : await probe(preferredPort);
    if (!target) {
      if (preferredPort !== null) forgetPort(preferredPort);
      const controllers = Array.from({ length: 10 }, () => new AbortController());
      target = await new Promise((resolve) => {
        let remaining = controllers.length;
        let settled = false;
        controllers.forEach((controller, index) => {
          void probe(49320 + index, controller).then((candidate) => {
            if (candidate && !settled) {
              settled = true;
              controllers.forEach((other) => { if (other !== controller) other.abort(); });
              resolve(candidate);
            }
            remaining -= 1;
            if (remaining === 0 && !settled) resolve(null);
          });
        });
      });
    }
    if (!target) return { ok: false, retryable: true };
    rememberPort(target.port);
    if (target.status === "delivered") return { ok: true };
    try {
      const response = await request(target.base, {
        method: "POST",
        headers: { "Content-Type": "application/json", Authorization: `Bearer ${secret}` },
        body,
      }, 5000);
      if (response.status === 200) return { ok: true };
      if (response.status === 202) return { ok: false, retryable: true };
      if (response.status === 413) return { ok: false, error: "tooLarge" };
      if (response.status === 400 || response.status === 409) return { ok: false, error: "invalid" };
      forgetPort(target.port);
      return { ok: false, retryable: true };
    } catch {
      forgetPort(target.port);
      return { ok: false, retryable: true };
    }
  };
})();
