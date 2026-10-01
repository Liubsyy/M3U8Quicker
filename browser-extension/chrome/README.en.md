# Chrome Extension

[简体中文](./README.md) | **English**

This is a minimal Chrome Manifest V3 extension.

## Load the extension

1. Open `chrome://extensions/`.
2. Enable **Developer mode**.
3. Click **Load unpacked**.
4. Select the `browser-extension/chrome` directory.

## Behavior

- The background script uses `webRequest` to detect `.m3u8` network requests, including requests from iframes.
- The extension also validates a page's `<video>` URLs when `currentSrc` or `src` contains `.m3u8`.
- After validation, a **M3U8 Quicker** button appears in the upper-right corner. Its icon matches the desktop application's `src-tauri/icons/icon.png`.
- Drag the button to reposition it. Only clicking launches a download; dragging does not.
- Use **中 / EN** in the panel's upper-right corner to switch languages immediately. Both Chrome and Firefox versions support this. Initially, Chinese browser UI languages use Simplified Chinese; all other languages use English. A manual selection is saved within the extension and synchronized across pages. Switching languages preserves selected videos and quality options.
- The extension opens the desktop application through `m3u8quicker://new-task?url=...&extra_headers=...` and automatically opens the **New Download** dialog.
- These headers are filled in automatically:
  - `referer`: the current page's full URL.
  - `origin`: the current page's origin.
  - `user-agent`: the browser's `navigator.userAgent`.
- Cookies are not forwarded automatically. In particular, `HttpOnly` cookies cannot be reliably read from the extension's content script context.

## Files

- `manifest.json`: extension manifest.
- `background.js`: background service worker that captures network requests through `webRequest` and forwards them to the page.
- `icon.png`: the desktop application's icon.
- `content.js`: receives detection results, injects site-specific scripts as needed, validates resources, and renders the button and panel.
- `injects/registry.js`: maps site domains to their injection scripts.
- `injects/bilibili.js`: reads `__playinfo__` from Bilibili pages and produces a DASH manifest.
