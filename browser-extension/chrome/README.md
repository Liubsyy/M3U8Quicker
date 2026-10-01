# Chrome Extension

**简体中文** | [English](./README.en.md)

这是一个最小可用的 Chrome Manifest V3 扩展。

加载方式：

1. 打开 `chrome://extensions/`
2. 开启“开发者模式”
3. 选择“加载已解压的扩展程序”
4. 选择 `browser-extension/chrome` 目录

行为说明：

- 扩展会通过后台 `webRequest` 监听浏览器网络层中的 `.m3u8` 请求，包含 iframe 内的请求
- 当页面中的 `<video>` 元素 `currentSrc` 或 `src` 含有 `.m3u8` 时，扩展也会校验该地址
- 校验通过后，页面右上角会出现按钮“M3U8 Quicker”，按钮图标与桌面端 `src-tauri/icons/icon.png` 保持一致
- 按钮支持拖动调整位置；只有点击才会触发唤起下载，拖动不会触发
- 面板右上角支持“中 / EN”即时切换，Chrome 和 Firefox 版本均支持。首次跟随浏览器界面语言（中文使用简体中文，其他语言使用英文），手动选择后在扩展内保存并同步到其他页面；切换语言保留当前勾选和清晰度。
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
- `content.js`：接收后台检测结果、按需注入站点脚本、校验与按钮 UI
- `injects/registry.js`：站点注入清单，按域名映射到对应注入脚本
- `injects/bilibili.js`：注入到 bilibili 页面读取 `__playinfo__`，输出 DASH manifest
