# Test HLS Server

这是一个独立于主项目的本地测试服务器，用来把视频切成 HLS 和 DASH 测试流，方便给 `m3u8quicker` 做下载联调。

## 特点

- 独立目录，不接入主应用打包
- 使用 Rust 编写 HTTP 服务
- 提供网页，可上传视频或直接选择本地视频文件
- 首页生成 HLS 输出，包含普通流、AES-128、AES-192、AES-256 四套播放列表
- `/dash` 页面单独生成 DASH 输出
- `/mp4` 页面把本机已有 MP4 通过本地端口暴露成 Direct MP4 测试地址
- `/live` 页面选择本机视频后预切片，生成一个会随时间滚动播放该视频的 HLS 直播模拟地址（TS 与 fMP4 两种）
- 生成后可直接访问 `.m3u8`、`.ts`、AES key、`.mpd`、`.m4s`，以及用于粘贴测试的 DASH JSON

## 环境要求

- 已安装 Rust / Cargo
- 已安装 `ffmpeg`

## 目录结构

- `src/main.rs`：服务入口
- `data/`：生成后的 HLS 文件，运行时自动创建
- `tmp/`：上传临时文件，运行时自动创建
- `Cargo.toml`：独立 Rust 项目配置
- `README.md`：使用说明

## 快速开始

先确保本机安装了 `ffmpeg` 并可在命令行中直接执行：

```bash
ffmpeg -version
```

启动服务：

```bash
cargo run --manifest-path Cargo.toml
```

默认地址：

```text
http://127.0.0.1:7878
```

## 页面功能

- 上传一个本地视频文件并切片
- 直接选择一个本地视频文件并切片
- 在页面里直接选择普通流、AES-128、AES-192、AES-256
- 在 `/dash` 页面里单独生成并复制 DASH 测试地址
- 在 `/mp4` 页面点按钮选择本机 MP4 后直接播放，不上传、不转码
- 查看已生成任务
- 首页打开或下载对应的各类 `index*.m3u8`
- `/dash` 页面打开或下载 `manifest.mpd`，并查看 `manifest.json`

## MP4 自动重试复现用例

启动服务后打开 [复现页面](http://127.0.0.1:7878/mp4/retry-test)，复制页面生成的独立下载地址。
也可手动使用 `http://127.0.0.1:7878/mp4/retry-test/case-001/video.mp4`，每轮更换 `case-001` 即可重新触发故障。

1. 在应用中新建 MP4 下载任务，名称填 `retry-test.mp4`，保存到空目录。
2. 等待自动重试，不要手动暂停/继续，不要打开播放器或提前在浏览器访问下载地址。
3. 首次下载约 2 秒后断开；按当前应用逻辑约 5 秒后自动重试。
4. 缺陷存在时，目录留下约 2 MiB 的 `retry-test.mp4.partial`，并生成完整 8 MiB 的 `retry-test (1).mp4`。
5. 修复后的预期是复用原临时文件，发送 `Range: bytes=2097152-`，最终只生成 `retry-test.mp4`。

服务行为：

- 模拟数据固定为 8 MiB，字节内容为绝对偏移量 `% 251`；扩展名和 Content-Type 为 MP4，但**内容不是可播放视频**。
- 无需准备本地 MP4，也不调用 FFmpeg；不影响原 `/mp4/local-file.mp4` 地址。
- 每个用例 ID 的首次无 Range GET 声明完整 Content-Length，但只发送 2 MiB，然后以响应体错误断开连接。
- 后续普通 GET 正常返回 200 和完整内容；合法的单段 Range 返回 206、Content-Range 和对应数据；无效或越界 Range 返回 416。
- HEAD 返回元信息，不消耗故障机会；Range 请求也不消耗首次无 Range GET 的故障机会。
- 传输约 1 MiB/s；终端输出用例 ID、请求序号、Range、响应状态及主动中断/完成信息。
- 用例状态在内存中，重启服务会重置。每轮测试应使用新用例 ID 和空保存目录；一个用例地址只交给一个下载任务。

典型缺陷日志：

```text
[mp4-retry case-001 #1] GET Range=<none> -> 200 ... inject_failure=true
[mp4-retry case-001 #1] 故意中断：已发送 2097152/8388608 字节
[mp4-retry case-001 #2] GET Range=<none> -> 200 ... inject_failure=false
```

正确续传时，第二次请求应为 `Range=bytes=2097152- -> 206 start=2097152 Content-Length=6291456`。

## 其他说明

- 该服务不会被主项目自动打包进去。
- 当前实现依赖系统 `ffmpeg` 完成切片。
- 默认输出是 VOD HLS，切片时长约 6 秒。
- 本地视频模式直接选择单个视频文件，不需要手写路径。
- 首页 HLS 任务默认生成四套播放列表：
  - `index.m3u8`：普通未加密流
  - `index-aes128.m3u8`：AES-128 加密流
  - `index-aes192.m3u8`：AES-192 加密测试流
  - `index-aes256.m3u8`：AES-256 加密测试流
- `/dash` 页面生成独立 DASH 测试地址：
  - `http://127.0.0.1:7878/dash-test/<job_id>/manifest.mpd`
  - `http://127.0.0.1:7878/dash-test/<job_id>/manifest.json`
- `/mp4` 页面提供本机 MP4 的端口代理：
  - 页面：`http://127.0.0.1:7878/mp4`
  - 直链：`http://127.0.0.1:7878/mp4/local-file.mp4`
  - 页面里的“浏览选择”按钮会在 Windows 上打开系统文件选择框
  - 可选启动环境变量：`TEST_HLS_SERVER_MP4_PATH=D:\Videos\sample.mp4`
- `/live` 页面提供 HLS 直播模拟（循环播放选择的视频以构造滚动窗口 m3u8）：
  - 页面：`http://127.0.0.1:7878/live`
  - TS 直播 URL：`http://127.0.0.1:7878/live-test/<job_id>/ts/index.m3u8`
  - fMP4 直播 URL：`http://127.0.0.1:7878/live-test/<job_id>/fmp4/index.m3u8`
  - 播放列表不带 `#EXT-X-ENDLIST`，`#EXT-X-MEDIA-SEQUENCE` 随服务器时间递增
  - 可选启动环境变量：`TEST_HLS_SERVER_LIVE_PATH=/path/to/sample.mp4`
  - 切片产物保留在 `data/live_<job_id>/{ts,fmp4}/` 下；任务列表会在服务进程退出后失效（不持久化）
- 其中 AES-192 / AES-256 主要用于当前仓库下载逻辑联调，浏览器原生或 `hls.js` 未必能正常在线播放。
- DASH 测试流用于当前仓库未加密 VOD DASH 下载联调，不覆盖 DRM 或 live/dynamic MPD。
