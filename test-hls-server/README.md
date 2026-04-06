# Test HLS Server

这是一个独立于主项目的本地测试服务器，用来把视频切成 `TS + M3U8`，方便给 `m3u8quicker` 做下载联调。

## 特点

- 独立目录，不接入主应用打包
- 使用 Rust 编写 HTTP 服务
- 提供网页，可上传视频或填写本地视频路径
- 调用本机 `ffmpeg` 生成 HLS 输出
- 生成后可直接访问 `.m3u8` 和 `.ts`

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
- 输入一个本机已有视频文件路径并切片
- 查看已生成任务
- 打开或下载对应的 `index.m3u8`

## 说明

- 该服务不会被主项目自动打包进去。
- 当前实现依赖系统 `ffmpeg` 完成切片。
- 默认输出是 VOD HLS，切片时长约 6 秒。
