import { useEffect, useState } from "react";
import {
  Input,
  InputNumber,
  Modal,
  Radio,
  Space,
  Switch,
  Tabs,
  Typography,
  message,
} from "antd";
import {
  getAppSettings,
  setDownloadConcurrency,
  setDownloadOutputSettings,
  setDownloadSpeedLimit,
  setProxySettings,
} from "../services/api";
import type { ProxySettings, ThemeMode } from "../types/settings";

const MIN_DOWNLOAD_CONCURRENCY = 1;
const MAX_DOWNLOAD_CONCURRENCY = 64;
const DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS = 1024;

type SpeedLimitMode = "unlimited" | "limited";

interface SettingsModalProps {
  open: boolean;
  themeMode: ThemeMode;
  onClose: () => void;
  onThemeModeChange: (mode: ThemeMode) => void;
}

export function SettingsModal({
  open,
  themeMode,
  onClose,
  onThemeModeChange,
}: SettingsModalProps) {
  const [proxySettings, setProxySettingsState] = useState<ProxySettings | null>(
    null
  );
  const [downloadConcurrency, setDownloadConcurrencyState] = useState<
    number | null
  >(null);
  const [savedDownloadConcurrency, setSavedDownloadConcurrency] = useState<
    number | null
  >(null);
  const [speedLimitMode, setSpeedLimitMode] =
    useState<SpeedLimitMode>("unlimited");
  const [downloadSpeedLimitKbps, setDownloadSpeedLimitKbps] =
    useState<number | null>(DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS);
  const [savedDownloadSpeedLimitKbps, setSavedDownloadSpeedLimitKbps] =
    useState<number>(0);
  const [deleteTsTempDirAfterDownload, setDeleteTsTempDirAfterDownload] =
    useState(false);
  const [convertToMp4, setConvertToMp4] = useState(true);
  const [loading, setLoading] = useState(false);
  const [savingProxy, setSavingProxy] = useState(false);
  const [savingConcurrency, setSavingConcurrency] = useState(false);
  const [savingSpeedLimit, setSavingSpeedLimit] = useState(false);
  const [savingDownloadOutput, setSavingDownloadOutput] = useState(false);

  useEffect(() => {
    if (!open) return;

    setLoading(true);
    getAppSettings()
      .then((settings) => {
        setProxySettingsState(settings.proxy);
        setDownloadConcurrencyState(settings.download_concurrency);
        setSavedDownloadConcurrency(settings.download_concurrency);
        setSavedDownloadSpeedLimitKbps(settings.download_speed_limit_kbps);
        setSpeedLimitMode(
          settings.download_speed_limit_kbps > 0 ? "limited" : "unlimited"
        );
        setDownloadSpeedLimitKbps(
          settings.download_speed_limit_kbps > 0
            ? settings.download_speed_limit_kbps
            : DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS
        );
        setDeleteTsTempDirAfterDownload(
          settings.delete_ts_temp_dir_after_download
        );
        setConvertToMp4(settings.convert_to_mp4);
      })
      .catch((error) => {
        message.error(`读取设置失败：${formatSettingsError(error)}`);
      })
      .finally(() => setLoading(false));
  }, [open]);

  const updateProxy = async (nextProxy: ProxySettings) => {
    setProxySettingsState(nextProxy);
    setSavingProxy(true);

    try {
      await setProxySettings(nextProxy);
      message.success("代理设置已保存");
    } catch (error) {
      message.error(`保存代理设置失败：${formatSettingsError(error)}`);
      const settings = await getAppSettings();
      setProxySettingsState(settings.proxy);
    } finally {
      setSavingProxy(false);
    }
  };

  const saveDownloadConcurrencyValue = async (value: number) => {
    const normalizedValue = Math.max(
      MIN_DOWNLOAD_CONCURRENCY,
      Math.min(MAX_DOWNLOAD_CONCURRENCY, Math.trunc(value))
    );

    if (savedDownloadConcurrency === normalizedValue) {
      setDownloadConcurrencyState(normalizedValue);
      return;
    }

    setDownloadConcurrencyState(normalizedValue);
    setSavingConcurrency(true);

    try {
      await setDownloadConcurrency(normalizedValue);
      setSavedDownloadConcurrency(normalizedValue);
      message.success("下载并发数量已保存");
    } catch (error) {
      message.error(`保存下载并发数量失败：${formatSettingsError(error)}`);
      const settings = await getAppSettings();
      setDownloadConcurrencyState(settings.download_concurrency);
      setSavedDownloadConcurrency(settings.download_concurrency);
    } finally {
      setSavingConcurrency(false);
    }
  };

  const saveDownloadSpeedLimitValue = async (value: number | null) => {
    const normalizedValue = Math.max(
      1,
      Math.trunc(value ?? DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS)
    );

    if (
      speedLimitMode === "limited" &&
      savedDownloadSpeedLimitKbps === normalizedValue
    ) {
      setDownloadSpeedLimitKbps(normalizedValue);
      return;
    }

    setSpeedLimitMode("limited");
    setDownloadSpeedLimitKbps(normalizedValue);
    setSavingSpeedLimit(true);

    try {
      await setDownloadSpeedLimit(normalizedValue);
      setSavedDownloadSpeedLimitKbps(normalizedValue);
      message.success("下载限速已保存");
    } catch (error) {
      message.error(`保存下载限速失败：${formatSettingsError(error)}`);
      const settings = await getAppSettings();
      setSavedDownloadSpeedLimitKbps(settings.download_speed_limit_kbps);
      setSpeedLimitMode(
        settings.download_speed_limit_kbps > 0 ? "limited" : "unlimited"
      );
      setDownloadSpeedLimitKbps(
        settings.download_speed_limit_kbps > 0
          ? settings.download_speed_limit_kbps
          : DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS
      );
    } finally {
      setSavingSpeedLimit(false);
    }
  };

  const updateSpeedLimitMode = async (nextMode: SpeedLimitMode) => {
    if (nextMode === speedLimitMode) return;

    if (nextMode === "unlimited") {
      setSpeedLimitMode("unlimited");
      setSavingSpeedLimit(true);
      try {
        await setDownloadSpeedLimit(0);
        setSavedDownloadSpeedLimitKbps(0);
        message.success("下载限速已关闭");
      } catch (error) {
        message.error(`保存下载限速失败：${formatSettingsError(error)}`);
        const settings = await getAppSettings();
        setSavedDownloadSpeedLimitKbps(settings.download_speed_limit_kbps);
        setSpeedLimitMode(
          settings.download_speed_limit_kbps > 0 ? "limited" : "unlimited"
        );
        setDownloadSpeedLimitKbps(
          settings.download_speed_limit_kbps > 0
            ? settings.download_speed_limit_kbps
            : DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS
        );
      } finally {
        setSavingSpeedLimit(false);
      }
      return;
    }

    await saveDownloadSpeedLimitValue(
      downloadSpeedLimitKbps !== null && downloadSpeedLimitKbps > 0
        ? downloadSpeedLimitKbps
        : DEFAULT_LIMITED_DOWNLOAD_SPEED_KBPS
    );
  };

  const updateDownloadOutputSettings = async (
    nextDeleteTsTempDirAfterDownload: boolean,
    nextConvertToMp4: boolean
  ) => {
    setDeleteTsTempDirAfterDownload(nextDeleteTsTempDirAfterDownload);
    setConvertToMp4(nextConvertToMp4);
    setSavingDownloadOutput(true);

    try {
      await setDownloadOutputSettings(
        nextDeleteTsTempDirAfterDownload,
        nextConvertToMp4
      );
      message.success("下载完成行为已保存");
    } catch (error) {
      message.error(`保存下载完成行为失败：${formatSettingsError(error)}`);
      const settings = await getAppSettings();
      setDeleteTsTempDirAfterDownload(
        settings.delete_ts_temp_dir_after_download
      );
      setConvertToMp4(settings.convert_to_mp4);
    } finally {
      setSavingDownloadOutput(false);
    }
  };

  const handleConfirm = async () => {
    if (
      downloadConcurrency !== null &&
      downloadConcurrency !== savedDownloadConcurrency
    ) {
      await saveDownloadConcurrencyValue(downloadConcurrency);
    }
    if (
      speedLimitMode === "limited" &&
      downloadSpeedLimitKbps !== savedDownloadSpeedLimitKbps
    ) {
      await saveDownloadSpeedLimitValue(downloadSpeedLimitKbps);
    }

    onClose();
  };

  const settingsTabItems = [
    {
      key: "general",
      label: "常规",
      children: (
        <Space direction="vertical" size={18} style={{ width: "100%" }}>
          <Typography.Text strong>主题</Typography.Text>
          <Radio.Group
            value={themeMode}
            onChange={(event) => onThemeModeChange(event.target.value)}
          >
            <Space size={20}>
              <Radio value="light">
                {themeMode === "light" ? "浅色（当前）" : "浅色"}
              </Radio>
              <Radio value="dark">
                {themeMode === "dark" ? "深色（当前）" : "深色"}
              </Radio>
            </Space>
          </Radio.Group>

          <Space direction="vertical" size={10} style={{ width: "100%" }}>
            <Typography.Text strong>代理设置</Typography.Text>
            <Space style={{ width: "100%", justifyContent: "space-between" }}>
              <Typography.Text>启用代理</Typography.Text>
              <Switch
                checked={proxySettings?.enabled ?? false}
                loading={loading || savingProxy}
                onChange={(checked) =>
                  proxySettings &&
                  void updateProxy({ ...proxySettings, enabled: checked })
                }
              />
            </Space>
            <Input
              value={proxySettings?.url ?? ""}
              placeholder="请输入代理地址"
              disabled={!proxySettings || loading || savingProxy}
              onBlur={(event) => {
                if (!proxySettings) return;
                const nextUrl = event.target.value.trim();
                if (nextUrl === proxySettings.url) return;
                void updateProxy({ ...proxySettings, url: nextUrl });
              }}
              onChange={(event) =>
                proxySettings &&
                setProxySettingsState({
                  ...proxySettings,
                  url: event.target.value,
                })
              }
            />
          </Space>
        </Space>
      ),
    },
    {
      key: "download",
      label: "下载设置",
      children: (
        <Space direction="vertical" size={18} style={{ width: "100%" }}>
          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <Typography.Text strong>下载并发数量</Typography.Text>
            <InputNumber
              min={MIN_DOWNLOAD_CONCURRENCY}
              max={MAX_DOWNLOAD_CONCURRENCY}
              precision={0}
              value={downloadConcurrency ?? undefined}
              style={{ width: 180 }}
              disabled={loading || savingConcurrency}
              placeholder="请输入下载并发数量"
              onChange={(value) =>
                setDownloadConcurrencyState(
                  typeof value === "number" ? value : null
                )
              }
              onBlur={() => {
                if (downloadConcurrency === null) {
                  setDownloadConcurrencyState(savedDownloadConcurrency);
                  return;
                }
                void saveDownloadConcurrencyValue(downloadConcurrency);
              }}
            />
          </Space>
          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <Typography.Text strong>下载限速</Typography.Text>
            <Radio.Group
              value={speedLimitMode}
              disabled={loading || savingSpeedLimit}
              onChange={(event) =>
                void updateSpeedLimitMode(event.target.value as SpeedLimitMode)
              }
            >
              <Radio.Button value="unlimited">不限速</Radio.Button>
              <Radio.Button value="limited">限速</Radio.Button>
            </Radio.Group>
            <InputNumber
              min={1}
              precision={0}
              addonAfter="KB/s"
              value={downloadSpeedLimitKbps ?? undefined}
              style={{ width: "100%" }}
              disabled={
                loading || savingSpeedLimit || speedLimitMode === "unlimited"
              }
              placeholder="请输入下载限速"
              onChange={(value) =>
                setDownloadSpeedLimitKbps(
                  typeof value === "number" ? value : null
                )
              }
              onBlur={() => {
                if (speedLimitMode !== "limited") return;
                void saveDownloadSpeedLimitValue(downloadSpeedLimitKbps);
              }}
            />
          </Space>
          <Space direction="vertical" size={8} style={{ width: "100%" }}>
            <Typography.Text strong>下载完成后</Typography.Text>
            <Space size={24}>
              <Space size={12}>
                <Typography.Text>删除 ts</Typography.Text>
                <Switch
                  checked={deleteTsTempDirAfterDownload}
                  loading={loading || savingDownloadOutput}
                  onChange={(checked) =>
                    void updateDownloadOutputSettings(checked, convertToMp4)
                  }
                />
              </Space>
              <Space size={12}>
                <Typography.Text>合并 MP4</Typography.Text>
                <Switch
                  checked={convertToMp4}
                  loading={loading || savingDownloadOutput}
                  onChange={(checked) =>
                    void updateDownloadOutputSettings(
                      deleteTsTempDirAfterDownload,
                      checked
                    )
                  }
                />
              </Space>
            </Space>
          </Space>
        </Space>
      ),
    },
  ];

  return (
    <Modal
      title="设置"
      open={open}
      onCancel={onClose}
      onOk={() => void handleConfirm()}
      okText="确定"
      cancelButtonProps={{ style: { display: "none" } }}
      width={500}
      confirmLoading={
        loading ||
        savingProxy ||
        savingConcurrency ||
        savingSpeedLimit ||
        savingDownloadOutput
      }
    >
      <Tabs
        className="settings-modal-tabs"
        defaultActiveKey="general"
        items={settingsTabItems}
      />
    </Modal>
  );
}

function formatSettingsError(error: unknown) {
  const text = String(error ?? "").trim();
  if (!text) {
    return "未知错误";
  }

  const normalized = text
    .replace(
      /^(Invalid input|M3U8 parse error|Network error|IO error|URL parse error|Decryption error|Conversion error|Failed to create HTTP client):\s*/i,
      ""
    )
    .replace(/^builder error:\s*/i, "")
    .trim();

  if (!normalized) {
    return "未知错误";
  }

  if (/^代理地址不能为空$/i.test(normalized)) {
    return normalized;
  }

  if (/^代理地址无效[:：]\s*/i.test(normalized)) {
    const detail = normalized.replace(/^代理地址无效[:：]\s*/i, "").trim();
    return formatProxyAddressDetail(detail);
  }

  return formatProxyAddressDetail(normalized);
}

function formatProxyAddressDetail(detail: string) {
  const normalizedDetail = detail
    .replace(/^(builder error:\s*)+/i, "")
    .trim();

  if (!normalizedDetail) {
    return "请输入有效的地址";
  }

  if (/builder error/i.test(normalizedDetail)) {
    return "代理地址端口无效";
  }

  if (/^relative url without a base$/i.test(normalizedDetail)) {
    return "请输入完整的代理地址，例如 http://127.0.0.1:7890";
  }

  if (/unknown proxy scheme/i.test(normalizedDetail)) {
    return "代理协议不受支持，请使用 http://、https:// 或 socks5://";
  }

  if (/empty host/i.test(normalizedDetail)) {
    return "代理地址缺少主机名";
  }

  if (/invalid port number/i.test(normalizedDetail)) {
    return "代理地址端口无效";
  }

  if (/failed to create http client/i.test(normalizedDetail)) {
    return "代理地址端口无效";
  }

  return normalizedDetail;
}
