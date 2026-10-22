/**
 * DeepSeek Harness（DSH）供应商预设。
 *
 * DSH 的供应商不是一个配置文件，而是 profile 里的路由行：第三方端点写进
 * `llm-pi-ai` 的 `providers.<route>`，官方 DeepSeek 端点写进 `llm-deepseek`。
 * 预设只填 `settingsConfig` 的结构字段，Key 一律留空由用户自己填。
 */
import type { ProviderCategory } from "../types";
import type { PresetFamilyFields } from "./presetFamilies";
import type { PresetTheme } from "./claudeProviderPresets";

/** DSH `llm-pi-ai` 支持的三种线上协议。 */
export type DshApiFormat =
  | "anthropic-messages"
  | "openai-completions"
  | "openai-responses";

export const DSH_API_FORMATS: ReadonlyArray<{
  value: DshApiFormat;
  label: string;
}> = [
  { value: "anthropic-messages", label: "Anthropic Messages" },
  { value: "openai-completions", label: "OpenAI Chat Completions" },
  { value: "openai-responses", label: "OpenAI Responses" },
];

/** 默认模型指针允许的思考强度（与 DSH 的 `reasoningEffort` 对齐）。 */
export type DshReasoningEffort = "off" | "low" | "high" | "max";

export const DSH_REASONING_EFFORTS: ReadonlyArray<DshReasoningEffort> = [
  "off",
  "low",
  "high",
  "max",
];

export interface DshPresetModel {
  id: string;
  name?: string;
  contextWindow?: number;
  maxTokens?: number;
  input?: Array<"text" | "image">;
}

export interface DshProviderPreset extends PresetFamilyFields {
  name: string;
  nameKey?: string;
  websiteUrl?: string;
  category?: ProviderCategory;
  theme?: PresetTheme;
  icon?: string;
  iconColor?: string;
  settingsConfig: {
    /** `deepseek-official` 写官方行；`custom` 写 `llm-pi-ai`。 */
    kind: "custom" | "deepseek-official";
    route?: string;
    displayName?: string;
    api?: DshApiFormat;
    baseURL: string;
    models: DshPresetModel[];
    reasoningEffort?: DshReasoningEffort;
  };
}

const DEEPSEEK_MODELS: DshPresetModel[] = [
  {
    id: "deepseek-chat",
    name: "DeepSeek Chat",
    contextWindow: 128000,
    maxTokens: 8192,
    input: ["text"],
  },
  {
    id: "deepseek-reasoner",
    name: "DeepSeek Reasoner",
    contextWindow: 128000,
    maxTokens: 65536,
    input: ["text"],
  },
];

export const dshProviderPresets: DshProviderPreset[] = [
  {
    name: "DeepSeek 官方",
    nameKey: "dsh.preset.official",
    websiteUrl: "https://platform.deepseek.com",
    category: "official",
    icon: "deepseek",
    iconColor: "#4D6BFE",
    settingsConfig: {
      kind: "deepseek-official",
      displayName: "DeepSeek",
      baseURL: "https://api.deepseek.com/anthropic",
      models: DEEPSEEK_MODELS,
      reasoningEffort: "high",
    },
  },
  {
    name: "Anthropic 兼容网关",
    nameKey: "dsh.preset.anthropicCompatible",
    category: "third_party",
    settingsConfig: {
      kind: "custom",
      route: "",
      api: "anthropic-messages",
      baseURL: "",
      models: [],
      reasoningEffort: "high",
    },
  },
  {
    name: "OpenAI 兼容网关",
    nameKey: "dsh.preset.openaiCompatible",
    category: "third_party",
    settingsConfig: {
      kind: "custom",
      route: "",
      api: "openai-completions",
      baseURL: "",
      models: [],
      reasoningEffort: "high",
    },
  },
];
