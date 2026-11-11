import { Input, Typography, theme } from "antd";
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
  return (
    <div style={{
      display: "flex",
      alignItems: "center",
      justifyContent: "flex-end",
      gap: 16,
      flexWrap: "wrap",
      marginBottom: 16,
    }}>
      {value.trim() && (
        <Typography.Text type="secondary" aria-live="polite">
          {loading ? t("searchingDownloads") : t("matchingDownloadCount", { value0: total })}
        </Typography.Text>
      )}
      <Input
        value={value}
        onChange={(event) => onChange(event.target.value)}
        prefix={<SearchOutlined style={{ color: token.colorTextPlaceholder }} />}
        placeholder={t("searchDownloadFilename")}
        aria-label={t("searchDownloadFilename")}
        allowClear
        style={{ width: 355, maxWidth: "100%" }}
      />
    </div>
  );
}
