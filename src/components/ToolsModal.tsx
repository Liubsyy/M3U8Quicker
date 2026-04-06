import { useEffect, useMemo, useState } from "react";
import { Button, Form, Input, Modal, Space, Typography, message } from "antd";
import {
  FileOutlined,
  FolderOpenOutlined,
  MergeCellsOutlined,
  SwapOutlined,
} from "@ant-design/icons";
import { open as pickDialogPath, save } from "@tauri-apps/plugin-dialog";
import { convertTsToMp4File, mergeTsFiles } from "../services/api";

export type ToolAction =
  | "merge-ts"
  | "ts-to-mp4"
  | "install-chrome-extension"
  | "install-edge-extension"
  | "install-firefox-extension";

interface ToolsModalProps {
  open: boolean;
  tool: ToolAction | null;
  onClose: () => void;
}

export function ToolsModal({ open, tool, onClose }: ToolsModalProps) {
  const [form] = Form.useForm();
  const [submitting, setSubmitting] = useState(false);

  const title = useMemo(() => {
    if (tool === "merge-ts") {
      return (
        <Space size={8}>
          <MergeCellsOutlined />
          <span>合并 ts</span>
        </Space>
      );
    }

    if (tool === "ts-to-mp4") {
      return (
        <Space size={8}>
          <SwapOutlined />
          <span>ts 转 mp4</span>
        </Space>
      );
    }

    return "工具";
  }, [tool]);

  useEffect(() => {
    if (!open) return;
    form.resetFields();
  }, [form, open, tool]);

  const handlePickInput = async () => {
    if (tool === "merge-ts") {
      const selected = await pickDialogPath({
        multiple: false,
        directory: true,
      });

      if (!selected) return;
      const inputDir = selected as string;
      form.setFieldValue("input_path", inputDir);
      if (!form.getFieldValue("output_path")) {
        form.setFieldValue("output_path", buildMergedOutputPath(inputDir));
      }
      return;
    }

    if (tool === "ts-to-mp4") {
      const selected = await pickDialogPath({
        multiple: false,
        directory: false,
        filters: [{ name: "TS 文件", extensions: ["ts"] }],
      });

      if (!selected) return;
      const inputPath = selected as string;
      form.setFieldValue("input_path", inputPath);
      if (!form.getFieldValue("output_path")) {
        form.setFieldValue("output_path", buildMp4OutputPath(inputPath));
      }
    }
  };

  const handlePickOutput = async () => {
    const currentOutput = form.getFieldValue("output_path") as string | undefined;
    const selected = await save({
      defaultPath: currentOutput,
      filters:
        tool === "merge-ts"
          ? [{ name: "TS 文件", extensions: ["ts"] }]
          : [{ name: "MP4 文件", extensions: ["mp4"] }],
    });

    if (selected) {
      form.setFieldValue("output_path", selected);
    }
  };

  const handleSubmit = async () => {
    if (!tool) return;

    try {
      const values = await form.validateFields();
      setSubmitting(true);
      const requestedOutput = values.output_path.trim();

      if (tool === "merge-ts") {
        const savedPath = await mergeTsFiles(values.input_path.trim(), requestedOutput);
        message.success(
          savedPath === requestedOutput
            ? "ts 已合并完成"
            : `ts 已合并完成，已另存为 ${getPathName(savedPath)}`
        );
      } else {
        const savedPath = await convertTsToMp4File(values.input_path.trim(), requestedOutput);
        message.success(
          savedPath === requestedOutput
            ? "mp4 已生成，原 ts 文件已保留"
            : `mp4 已生成，原 ts 文件已保留，已另存为 ${getPathName(savedPath)}`
        );
      }

      onClose();
    } catch (error: unknown) {
      if (error && typeof error === "object" && "errorFields" in error) return;
      message.error(`执行工具失败: ${error}`);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Modal
      title={title}
      open={open}
      onCancel={onClose}
      onOk={() => void handleSubmit()}
      okText="开始处理"
      cancelText="取消"
      confirmLoading={submitting}
      destroyOnClose
      width={520}
    >
      <Form form={form} layout="vertical">
        <Form.Item
          label={tool === "merge-ts" ? "TS 目录" : "TS 文件"}
          required
        >
          <Space.Compact style={{ width: "100%" }}>
            <Form.Item
              name="input_path"
              noStyle
              rules={[
                {
                  required: true,
                  message: tool === "merge-ts" ? "请选择 TS 目录" : "请选择 TS 文件",
                },
              ]}
            >
              <Input
                readOnly
                placeholder={
                  tool === "merge-ts" ? "请选择包含 ts 切片的目录" : "请选择待转换的 ts 文件"
                }
              />
            </Form.Item>
            <Button
              icon={tool === "merge-ts" ? <FolderOpenOutlined /> : <FileOutlined />}
              onClick={() => void handlePickInput()}
            >
              选择
            </Button>
          </Space.Compact>
        </Form.Item>

        <Form.Item label="输出文件" required>
          <Space.Compact style={{ width: "100%" }}>
            <Form.Item
              name="output_path"
              noStyle
              rules={[{ required: true, message: "请选择输出文件" }]}
            >
              <Input readOnly placeholder="请选择输出文件" />
            </Form.Item>
            <Button icon={<FolderOpenOutlined />} onClick={() => void handlePickOutput()}>
              选择
            </Button>
          </Space.Compact>
        </Form.Item>

        {tool === "ts-to-mp4" && (
          <Typography.Text type="secondary">
            该工具会保留原 ts 文件，只额外生成一个 mp4 文件。
          </Typography.Text>
        )}
      </Form>
    </Modal>
  );
}

function splitPath(path: string) {
  const normalized = path.replace(/\\/g, "/");
  const lastSlashIndex = normalized.lastIndexOf("/");
  const dir = lastSlashIndex >= 0 ? normalized.slice(0, lastSlashIndex) : "";
  const name = lastSlashIndex >= 0 ? normalized.slice(lastSlashIndex + 1) : normalized;
  return { dir, name };
}

function joinPath(dir: string, name: string) {
  if (!dir) return name;
  return `${dir}/${name}`;
}

function getPathName(path: string) {
  return splitPath(path).name || path;
}

function buildMergedOutputPath(inputDir: string) {
  const { dir, name } = splitPath(inputDir);
  const normalizedName = (name || "merged")
    .replace(/^\.+/, "")
    .replace(/^m3u8quicker_temp_/, "")
    .trim();
  return joinPath(dir, `${normalizedName || "merged"}.ts`);
}

function buildMp4OutputPath(inputPath: string) {
  const { dir, name } = splitPath(inputPath);
  const nextName = name.toLowerCase().endsWith(".ts")
    ? `${name.slice(0, -3)}.mp4`
    : `${name}.mp4`;
  return joinPath(dir, nextName);
}
