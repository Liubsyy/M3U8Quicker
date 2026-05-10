# Test HLS Server

这是一个独立于主项目的本地测试服务器，用来把视频切成 HLS 和 DASH 测试流，方便给 `m3u8quicker` 做下载联调。

## 特点

- 独立目录，不接入主应用打包
- 使用 Rust 编写 HTTP 服务
- 提供网页，可上传视频或直接选择本地视频文件
- 首页生成 HLS 输出，包含普通流、AES-128、AES-192、AES-256 四套播放列表
- `/dash` 页面单独生成 DASH 输出
- `/mp4` 页面把本机已有 MP4 通过本地端口暴露成 Direct MP4 测试地址
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
cargo run --manifest-path test-hls-server/Cargo.toml
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

## 说明

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
- 其中 AES-192 / AES-256 主要用于当前仓库下载逻辑联调，浏览器原生或 `hls.js` 未必能正常在线播放。
- DASH 测试流用于当前仓库未加密 VOD DASH 下载联调，不覆盖 DRM 或 live/dynamic MPD。
