# Chrome Extension

这是一个最小可用的 Chrome Manifest V3 扩展。

加载方式：

1. 打开 `chrome://extensions/`
2. 开启“开发者模式”
3. 选择“加载已解压的扩展程序”
4. 选择当前目录下的 `chrome-extension`

行为说明：

- 扩展会通过后台 `webRequest` 监听浏览器网络层中的 `.m3u8` 请求，包含 iframe 内的请求
- 当页面里的 `XMLHttpRequest` 或 `fetch` 请求 URL 含有 `.m3u8` 时，扩展也会在页面上下文中补充检测
- 当页面中的 `<video>` 元素 `currentSrc` 或 `src` 含有 `.m3u8` 时，扩展也会校验该地址
- 校验通过后，页面右上角会出现按钮“M3U8 Quicker”，按钮图标与桌面端 `src-tauri/icons/icon.png` 保持一致
- 按钮支持拖动调整位置；只有点击才会触发唤起下载，拖动不会触发
- 点击按钮会尝试通过 `m3u8quicker://new-task?url=...&extra_headers=...` 唤起桌面端，并自动打开“新建下载”弹窗
- 默认会预填这些 Header：
  - `referer:<当前页面完整地址>`
  - `origin:<当前页面 origin>`
  - `user-agent:<浏览器 navigator.userAgent>`
- 不会自动传递 `Cookie`，尤其 `HttpOnly` Cookie 无法从扩展安全上下文中可靠读取

目录结构：

- `manifest.json`：扩展清单
- `background.js`：后台 `service worker`，通过 `webRequest` 捕获网络请求并转发给页面
- `icon.png`：复用桌面端主图标
- `content.js`：接收后台检测结果、页面注入、校验与按钮 UI
- `injected-network.js`：注入页面上下文，拦截 `XMLHttpRequest` 与 `fetch`