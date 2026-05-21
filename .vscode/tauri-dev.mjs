// F5 / `npm run dev:desktop` 的统一入口。
//
// 背景：浏览器扩展通过自定义协议 `m3u8quicker://...` 唤起桌面端。
//   - Windows/Linux 在运行时注册协议（src-tauri/src/lib.rs 的 register_all），
//     所以 `tauri dev` 下也能跳转。
//   - macOS 只能在“打包时”通过 .app 的 Info.plist(CFBundleURLTypes) 注册协议，
//     而 `tauri dev` 只产出裸二进制（无 .app），Launch Services 找不到处理器，
//     于是浏览器跳转在 dev 下静默失败。
//
// 本脚本：
//   - 非 macOS：原样委托给 `tauri dev`（保持现有行为）。
//   - macOS：自起 Vite + cargo build 出 dev 二进制 → 组装一个带协议声明的 .app
//     → lsregister 注册 → 直接运行 bundle 内二进制。运行中的进程即拥有 bundle 身份
//     且被 LS 登记，扩展跳转时 LS 把 openURLs 直接投递给它，前端已有的 onOpenUrl
//     流程（src/App.tsx）正常触发。
//
// 已知限制（macOS dev）：
//   - Rust 改动需重按 F5（本模式不做 Rust 自动重建；前端 Vite HMR 不受影响）。
//   - 若本机已安装/正在运行打包版 M3U8 Quicker（同 bundle id），做深链调试时建议先退出它，
//     避免 LS 路由到打包版而非 dev 实例。

import { spawn } from "node:child_process";
import {
  existsSync,
  linkSync,
  copyFileSync,
  mkdirSync,
  rmSync,
  symlinkSync,
  writeFileSync,
  chmodSync,
} from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(__dirname, "..");

// 与 src-tauri/tauri.conf.json / Cargo.toml / vite.config.ts 对齐。
const APP_IDENTIFIER = "com.liubsyy.m3u8quicker";
const APP_PRODUCT_NAME = "M3U8 Quicker";
const BIN_NAME = "m3u8quicker";
const URL_SCHEME = "m3u8quicker";
const DEV_SERVER_URL = "http://127.0.0.1:1420";

const children = [];

function run(command, args, opts = {}) {
  const child = spawn(command, args, { stdio: "inherit", ...opts });
  children.push(child);
  return child;
}

function killChildren() {
  for (const child of children) {
    if (!child.killed) {
      try {
        child.kill("SIGTERM");
      } catch {
        // ignore
      }
    }
  }
}

function wireCleanup() {
  const onSignal = (signal) => {
    killChildren();
    process.exit(signal === "SIGINT" ? 130 : 143);
  };
  process.on("SIGINT", () => onSignal("SIGINT"));
  process.on("SIGTERM", () => onSignal("SIGTERM"));
  process.on("exit", killChildren);
}

// --- 非 macOS：直接委托 tauri dev ---------------------------------------------

function runTauriDev() {
  const child =
    process.platform === "win32"
      ? run("cmd.exe", ["/d", "/s", "/c", "npm run tauri dev"], { cwd: ROOT })
      : run("npm", ["run", "tauri", "dev"], { cwd: ROOT });
  child.on("exit", (code) => process.exit(code ?? 0));
}

// --- macOS：Vite + dev .app ---------------------------------------------------

async function waitForDevServer(url, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await fetch(url, { method: "GET" });
      return; // 任何 HTTP 响应都说明 Vite 已起来
    } catch {
      await new Promise((r) => setTimeout(r, 300));
    }
  }
  throw new Error(`Vite 开发服务器在 ${timeoutMs}ms 内未就绪: ${url}`);
}

function buildRustDev() {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = run(
      "cargo",
      ["build", "--manifest-path", join(ROOT, "src-tauri", "Cargo.toml")],
      { cwd: ROOT },
    );
    child.on("exit", (code) => {
      if (code === 0) {
        resolvePromise();
      } else {
        rejectPromise(new Error(`cargo build 失败，退出码 ${code}`));
      }
    });
  });
}

function infoPlist() {
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>${BIN_NAME}</string>
  <key>CFBundleIdentifier</key>
  <string>${APP_IDENTIFIER}</string>
  <key>CFBundleName</key>
  <string>${APP_PRODUCT_NAME}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleShortVersionString</key>
  <string>0.0.0-dev</string>
  <key>CFBundleVersion</key>
  <string>0.0.0-dev</string>
  <key>LSMinimumSystemVersion</key>
  <string>10.13</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key>
      <string>${APP_IDENTIFIER}</string>
      <key>CFBundleURLSchemes</key>
      <array>
        <string>${URL_SCHEME}</string>
      </array>
    </dict>
  </array>
</dict>
</plist>
`;
}

// 让 dev bundle 内某个相对路径软链到仓库内目标，best-effort。
function linkResource(target, linkPath) {
  if (!existsSync(target)) {
    return;
  }
  try {
    if (existsSync(linkPath)) {
      rmSync(linkPath, { recursive: true, force: true });
    }
    symlinkSync(target, linkPath);
  } catch (error) {
    console.warn(`[dev:desktop] 资源软链失败（忽略）: ${linkPath} -> ${error.message}`);
  }
}

function assembleDevBundle() {
  const builtBinary = join(ROOT, "src-tauri", "target", "debug", BIN_NAME);
  if (!existsSync(builtBinary)) {
    throw new Error(`未找到 dev 二进制: ${builtBinary}`);
  }

  const bundle = join(
    ROOT,
    "src-tauri",
    "target",
    "dev-deeplink",
    `${APP_PRODUCT_NAME}.app`,
  );
  const macOSDir = join(bundle, "Contents", "MacOS");
  const resourcesDir = join(bundle, "Contents", "Resources");

  // 整体重建 bundle，避免残留旧 inode / 旧 Info.plist。
  rmSync(bundle, { recursive: true, force: true });
  mkdirSync(macOSDir, { recursive: true });
  mkdirSync(resourcesDir, { recursive: true });

  writeFileSync(join(bundle, "Contents", "Info.plist"), infoPlist());

  // 可执行文件：优先 hardlink（同卷瞬时、保留 bundle 身份），失败回退为 copy。
  const innerBinary = join(macOSDir, BIN_NAME);
  try {
    linkSync(builtBinary, innerBinary);
  } catch {
    copyFileSync(builtBinary, innerBinary);
    chmodSync(innerBinary, 0o755);
  }

  // dev 模式下 macOS 的 resource_dir 是 Contents/Resources，软链扩展资源目录，
  // 让“安装扩展”命令在本模式下仍能定位资源。
  linkResource(
    join(ROOT, "browser-extension", "chrome"),
    join(resourcesDir, "chrome-extension"),
  );
  linkResource(
    join(ROOT, "browser-extension", "firefox"),
    join(resourcesDir, "firefox-extension"),
  );

  return { bundle, innerBinary };
}

function registerWithLaunchServices(bundle) {
  const lsregister =
    "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
  return new Promise((resolvePromise) => {
    const child = spawn(lsregister, ["-f", bundle], { stdio: "inherit" });
    child.on("error", (error) => {
      console.warn(`[dev:desktop] lsregister 调用失败（忽略）: ${error.message}`);
      resolvePromise();
    });
    child.on("exit", (code) => {
      if (code !== 0) {
        console.warn(`[dev:desktop] lsregister 返回非 0 (${code})，继续。`);
      }
      resolvePromise();
    });
  });
}

async function runMacDev() {
  console.log("[dev:desktop] macOS：启动 Vite + 注册 dev .app 以支持深链跳转。");

  // 1. Vite（前端 HMR）
  run("npm", ["run", "dev"], { cwd: ROOT });
  await waitForDevServer(DEV_SERVER_URL);
  console.log(`[dev:desktop] Vite 就绪：${DEV_SERVER_URL}`);

  // 2. 构建 dev 二进制（不含 custom-protocol 特性 → 加载 devUrl）
  await buildRustDev();

  // 3. 组装 dev .app
  const { bundle, innerBinary } = assembleDevBundle();
  console.log(`[dev:desktop] dev bundle: ${bundle}`);

  // 4. 向 Launch Services 注册协议处理器
  await registerWithLaunchServices(bundle);

  // 5. 直接运行 bundle 内二进制（拥有 bundle 身份 + 被 LS 登记 + 终端可见日志）
  console.log("[dev:desktop] 启动应用。Rust 改动后请重启（重新按 F5）。");
  const app = run(innerBinary, [], { cwd: ROOT });
  app.on("exit", (code) => {
    killChildren();
    process.exit(code ?? 0);
  });
}

// --- 入口 --------------------------------------------------------------------

wireCleanup();

if (process.platform === "darwin") {
  runMacDev().catch((error) => {
    console.error(`[dev:desktop] ${error.message}`);
    killChildren();
    process.exit(1);
  });
} else {
  runTauriDev();
}
