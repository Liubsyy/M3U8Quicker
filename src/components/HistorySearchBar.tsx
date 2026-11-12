import { Input, Tooltip, Typography, theme } from "antd";
import { SearchOutlined } from "@ant-design/icons";
import { t, useTranslation } from "../i18n";

interface HistorySearchBarProps {
  value: string;
  onChange: (value: string) => void;
  loading: boolean;
  total: number;
}

export function HistorySearchBar({ value, onChange, loading, total }: HistorySearchBarProps) {
  useTranslation();
  const { token } = theme.useToken();
  const resultText = loading ? t("searchingDownloads") : t("matchingDownloadCount", { value0: total });
  return (
    <div className="history-search-bar">
      <Input
        value={value}
        onChange={(event) => onChange(event.target.value)}
        prefix={<SearchOutlined style={{ color: token.colorTextPlaceholder }} />}
        placeholder={t("searchDownloadFilename")}
        aria-label={t("searchDownloadFilename")}
        allowClear
        suffix={value.trim() ? (
          <Tooltip title={resultText}>
            <Typography.Text type="secondary" aria-label={resultText} aria-live="polite">
              {loading ? "…" : total}
            </Typography.Text>
          </Tooltip>
        ) : undefined}
      />
    </div>
  );
}
