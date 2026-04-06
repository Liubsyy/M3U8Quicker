# M3U8 Quicker

<p align="center">
  <img src="./src-tauri/icons/icon.png" alt="M3U8 Quicker icon" width="96" />
</p>

**M3U8 Quicker** 是一个基于 `Tauri + Rust + React + TypeScript` 构建的桌面应用，用于下载、管理与播放 `M3U8` 视频，支持 Windows、macOS 和 Linux。

项目同时包含一个可选的 Chrome 扩展：当网页中检测到 `.m3u8` 请求时，可以快速唤起桌面应用并预填下载信息。

![下载主界面](./doc/img/download_main.png)

## 功能特性

- 支持下载 `M3U8` 格式的视频
- 支持多线程下载，提高下载效率
- 支持 AES-128 / AES-192 / AES-256 加密流的密钥拉取与解密
- 支持边下边播、暂停/继续下载（断点续传）、失败分片重试
- 自动合并ts和转.mp4格式
- 应用内工具：
  - `合并 ts`
  - `ts 转 mp4`
  - `安装 Chrome 扩展`
- 支持应用内代理设置、下载并发设置、下载完成行为设置

## 使用说明

### 安装

可在 GitHub Releases 页面下载对应的桌面安装包或发行文件，常见安装文件与平台对应关系如下：

| 文件类型 / 文件名示例 | 适用平台 | 说明 |
| --- | --- | --- |
| `M3U8.Quicker_xxx_x64-setup.exe` | Windows 64 位 | 推荐，大多数 Windows 电脑使用这个安装包 |
| `M3U8.Quicker_xxx_x86-setup.exe` | Windows 32 位 | 仅旧版 32 位 Windows 需要 |
| `M3U8.Quicker_xxx_aarch64.dmg` | macOS Apple Silicon | 适用于 M1 / M2 / M3 等 ARM 芯片 Mac |
| `M3U8.Quicker_xxx_x64.dmg` | macOS Intel | 适用于 Intel 芯片 Mac |
| `M3U8.Quicker_xxx_amd64.AppImage` | Linux x64 | 免安装，赋予执行权限后可直接运行 |
| `M3U8.Quicker_xxx_amd64.deb` | Debian / Ubuntu 系 | 适用于 `Ubuntu`、`Debian`、`Linux Mint` 等 |
| `M3U8.Quicker_xxx_x86_64.rpm` | RPM 系 Linux | 适用于 `Fedora`、`RHEL`、`CentOS Stream`、`openSUSE` 等 |

macOS 首次安装时如果遇到“无法打开”或“应用已损坏”之类的权限提示，可按下面方式处理：

- 先在“系统设置 -> 隐私与安全性”中找到被拦截的应用，点击“仍要打开”
- 或在 Finder 中对应用点右键，选择“打开”，再确认一次
- 如果仍被 Gatekeeper 拦截，可在终端执行 `xattr -rd com.apple.quarantine /Applications/M3U8\\ Quicker.app` 后重新打开

安装完成后首次启动应用，如果需要通过浏览器快速投递下载任务，可继续在应用内打开“工具 -> 安装 Chrome 扩展”查看引导。

### 新建下载

在主界面点击“新建下载”，填写 `M3U8` 地址后即可创建任务。

如果资源需要额外请求头（可选），可以在附加 Header 中填写，例如：

```text
referer:https://example.com
origin:https://example.com
```
![新建下载](./doc/img/newtask.png)


### 下载中

任务创建后会进入下载列表，下载过程中支持以下操作：

- 可手动暂停正在下载的任务
- 已暂停的任务可继续恢复下载，支持断点续传
- 下载失败的分片可重试下载失败分片
- 下载中的任务可直接打开播放器，边下边看
- 不再需要的任务可取消或移除

下载列表会持续显示任务状态、下载进度、下载速度等信息，便于随时查看当前下载情况。

![下载中](./doc/img/download_ts.png)


### 播放

- 下载中的任务支持直接打开播放器，便于边下边看
- 下载中若跳转到未下载分片进度时，优先下载当前播放进度分片
- 已完成任务支持播放最终文件

![播放窗口](./doc/img/playvideo.png)

### 设置

设置面板当前支持：

- 主题切换
- 代理开关与代理地址
- 下载并发数量
- 下载完成后是否删除临时 ts 目录
- 下载完成后是否自动转换为 mp4

## Chrome 扩展（可选）

安装Chrome扩展后，网页中会自动扫描出`.m3u8`链接和视频地址，右上角会出现一个按钮`M3U8 Quicker`，点击后唤起桌面应用新建下载任务，并自动带入 `url`、`referer`、`origin`、`user-agent`。
> 安装扩展引导：打开M3U8 Quicker-> 工具 -> 安装 Chrome 扩展，按引导安装Chrome扩展

![Chrome 扩展安装引导](./doc/img/chrome-extension.png)


使用限制：
- 不会自动传递 `Cookie`
- 无法可靠读取 `HttpOnly` Cookie

## 技术栈

- 前端：`React 19`、`TypeScript`、`Vite 8`、`Ant Design 6`
- 桌面端：`Tauri 2`
- 后端逻辑：`Rust`
- 网络请求：`reqwest`
- 本地播放服务：`axum`

## 环境要求

- Node.js：建议使用较新的 LTS 版本
- Rust：最低 `1.88`

## 目录结构

- `src/`：React 前端界面与页面逻辑
- `src-tauri/`：Tauri 桌面端与 Rust 后端实现
- `chrome-extension/`：可选的 Chrome 扩展源码
- `test-hls-server/`：独立的 Rust 本地测试服务，用于把视频切成 `m3u8 + ts`
- `public/`：静态资源

## 快速开始

### 1. 安装依赖

```bash
npm install
```

### 2. 启动前端开发服务器

```bash
npm run dev
```

### 3. 启动桌面应用

```bash
npm run tauri dev
```

## 常用命令

| 命令 | 说明 |
| --- | --- |
| `npm install` | 安装前端依赖 |
| `npm run dev` | 启动前端开发服务器 |
| `npm run preview` | 预览前端构建产物 |
| `npm run tauri dev` | 启动桌面应用开发模式 |
| `npm run lint` | 检查前端代码规范 |
| `npm run build` | 执行 TypeScript 检查并构建前端 |
| `npm run tauri build` | 构建桌面应用安装包 |
| `cargo check --manifest-path src-tauri/Cargo.toml` | 检查 Rust / Tauri 侧代码 |
| `cargo test --manifest-path src-tauri/Cargo.toml` | 运行 Rust 单元测试 |

## 打包与资源说明

- 应用名称：`M3U8 Quicker`
- 应用标识：`com.liubsyy.m3u8quicker`

> 桌面应用打包时会把仓库根目录下的 `chrome-extension/` 一并作为资源打入安装包，因此安装后的应用仍然可以为扩展安装引导提供目标目录。
