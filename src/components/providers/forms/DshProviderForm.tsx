import { useEffect, useMemo, useState } from "react";
import { useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { useTranslation } from "react-i18next";
import { Plus, RefreshCw, Trash2 } from "lucide-react";
import { toast } from "@/lib/toast";
import { fetchModelsForConfig } from "@/lib/api/model-fetch";

import { Button } from "@/components/ui/button";
import { Form } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { providerSchema, type ProviderFormData } from "@/lib/schemas/provider";
import {
  DSH_API_FORMATS,
  DSH_REASONING_EFFORTS,
  dshProviderPresets,
  type DshApiFormat,
  type DshProviderPreset,
  type DshReasoningEffort,
} from "@/config/dshProviderPresets";
import { BasicFormFields } from "./BasicFormFields";
import ApiKeyInput from "./ApiKeyInput";
import { ProviderPresetSelector } from "./ProviderPresetSelector";
import type { ProviderFormProps } from "./ProviderForm";

/** 与后端 `dsh_config::is_valid_route` / DSH 设置页 `ROUTE_PATTERN` 一致。 */
const ROUTE_PATTERN = /^[a-z][a-z0-9]*(-[a-z0-9]+)*$/;

interface DshModelDraft {
  key: string;
  id: string;
  name: string;
  contextWindow: string;
  maxTokens: string;
}

interface DshConfigDraft {
  kind: "custom" | "deepseek-official";
  route: string;
  api: DshApiFormat;
  baseURL: string;
  apiKey: string;
  /** 导入的行可能用了自定义凭据引用；表单不改它，保存时原样带回。 */
  apiKeyEnv?: string;
  models: DshModelDraft[];
  reasoningEffort: DshReasoningEffort;
}

const PRESET_ENTRIES = dshProviderPresets.map((preset, index) => ({
  id: String(index),
  preset,
}));

let modelKeySeed = 0;
const nextModelKey = () => `dsh-model-${(modelKeySeed += 1)}`;

const emptyModel = (): DshModelDraft => ({
  key: nextModelKey(),
  id: "",
  name: "",
  contextWindow: "",
  maxTokens: "",
});

const routeFromName = (name: string) =>
  name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .replace(/^([^a-z])/, "dsh-$1");

const isHttpUrl = (value: string) => {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
};

const draftFromSettings = (
  settings: Record<string, unknown> | undefined,
): DshConfigDraft => {
  const kind =
    settings?.kind === "deepseek-official" ? "deepseek-official" : "custom";
  const models = Array.isArray(settings?.models) ? settings.models : [];
  const drafts = models
    .filter(
      (model): model is Record<string, unknown> =>
        typeof model === "object" && model !== null,
    )
    .map((model) => ({
      key: nextModelKey(),
      id: typeof model.id === "string" ? model.id : "",
      name: typeof model.name === "string" ? model.name : "",
      contextWindow:
        typeof model.contextWindow === "number"
          ? String(model.contextWindow)
          : "",
      maxTokens:
        typeof model.maxTokens === "number" ? String(model.maxTokens) : "",
    }));
  const api = DSH_API_FORMATS.some((format) => format.value === settings?.api)
    ? (settings?.api as DshApiFormat)
    : "anthropic-messages";
  const effort = DSH_REASONING_EFFORTS.includes(
    settings?.reasoningEffort as DshReasoningEffort,
  )
    ? (settings?.reasoningEffort as DshReasoningEffort)
    : "high";
  return {
    kind,
    route: typeof settings?.route === "string" ? settings.route : "",
    api,
    baseURL: typeof settings?.baseURL === "string" ? settings.baseURL : "",
    apiKey: typeof settings?.apiKey === "string" ? settings.apiKey : "",
    apiKeyEnv:
      typeof settings?.apiKeyEnv === "string" && settings.apiKeyEnv.trim()
        ? settings.apiKeyEnv
        : undefined,
    models: drafts.length > 0 ? drafts : [emptyModel()],
    reasoningEffort: effort,
  };
};

const draftFromPreset = (preset: DshProviderPreset): DshConfigDraft => ({
  kind: preset.settingsConfig.kind,
  route: preset.settingsConfig.route ?? "",
  api: preset.settingsConfig.api ?? "anthropic-messages",
  baseURL: preset.settingsConfig.baseURL,
  apiKey: "",
  models:
    preset.settingsConfig.models.length > 0
      ? preset.settingsConfig.models.map((model) => ({
          key: nextModelKey(),
          id: model.id,
          name: model.name ?? "",
          contextWindow: model.contextWindow ? String(model.contextWindow) : "",
          maxTokens: model.maxTokens ? String(model.maxTokens) : "",
        }))
      : [emptyModel()],
  reasoningEffort: preset.settingsConfig.reasoningEffort ?? "high",
});

/**
 * DeepSeek Harness 供应商表单。
 *
 * DSH 的供应商是 profile 里的一行路由：官方卡写 `llm-deepseek`，第三方卡写
 * `llm-pi-ai.providers.<route>`；Key 另存进 `$DSH_HOME/.credentials.yaml`。表单只负责
 * 收集这些字段，落下三处由后端 `dsh_config` 一次事务完成。
 */
export function DshProviderForm({
  initialData,
  onSubmit,
  onCancel,
  submitLabel,
  showButtons = true,
  onSubmittingChange,
  onSubmitReadyChange,
}: ProviderFormProps) {
  const { t } = useTranslation();
  const isEdit = Boolean(initialData);
  const [config, setConfig] = useState<DshConfigDraft>(() =>
    draftFromSettings(initialData?.settingsConfig),
  );
  const [presetId, setPresetId] = useState("custom");
  const [routeTouched, setRouteTouched] = useState(isEdit);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const form = useForm<ProviderFormData>({
    resolver: zodResolver(providerSchema),
    defaultValues: {
      name: initialData?.name ?? "",
      notes: initialData?.notes ?? "",
      websiteUrl: initialData?.websiteUrl ?? "",
      icon: initialData?.icon ?? "",
      iconColor: initialData?.iconColor ?? "",
      settingsConfig: JSON.stringify(config),
    },
  });
  const name = form.watch("name");

  // 新卡没填 route 时跟着名字走，避免落库后拿到一个 UUID 路由。
  useEffect(() => {
    if (isEdit || routeTouched) return;
    const derived = routeFromName(name);
    setConfig((current) =>
      current.route === derived ? current : { ...current, route: derived },
    );
  }, [name, isEdit, routeTouched]);

  const routeInvalid =
    config.kind === "custom" &&
    config.route.length > 0 &&
    !ROUTE_PATTERN.test(config.route);
  const baseUrlInvalid =
    config.baseURL.trim().length > 0 && !isHttpUrl(config.baseURL.trim());
  const models = useMemo(
    () => config.models.filter((model) => model.id.trim().length > 0),
    [config.models],
  );
  const ready =
    name.trim().length > 0 &&
    config.baseURL.trim().length > 0 &&
    !baseUrlInvalid &&
    (config.kind === "deepseek-official" || config.route.trim().length > 0) &&
    !routeInvalid &&
    models.length > 0;

  useEffect(() => {
    onSubmitReadyChange?.(ready);
  }, [ready, onSubmitReadyChange]);

  const update = (patch: Partial<DshConfigDraft>) =>
    setConfig((current) => ({ ...current, ...patch }));

  const updateModel = (key: string, patch: Partial<DshModelDraft>) =>
    setConfig((current) => ({
      ...current,
      models: current.models.map((model) =>
        model.key === key ? { ...model, ...patch } : model,
      ),
    }));

  const choosePreset = (id: string) => {
    setPresetId(id);
    if (id === "custom") {
      const next = draftFromSettings(undefined);
      setConfig(next);
      setRouteTouched(false);
      form.reset({
        name: "",
        notes: "",
        websiteUrl: "",
        icon: "",
        iconColor: "",
        settingsConfig: JSON.stringify(next),
      });
      return;
    }
    const preset = dshProviderPresets[Number(id)];
    if (!preset) return;
    const next = draftFromPreset(preset);
    setConfig(next);
    setRouteTouched(Boolean(next.route));
    form.reset({
      name: preset.name,
      notes: "",
      websiteUrl: preset.websiteUrl ?? "",
      icon: preset.icon ?? "",
      iconColor: preset.iconColor ?? "",
      settingsConfig: JSON.stringify(next),
    });
  };

  const fetchModels = async () => {
    const baseUrl = config.baseURL.trim();
    if (!baseUrl || fetchingModels) return;
    setFetchingModels(true);
    try {
      const fetched = await fetchModelsForConfig(
        baseUrl,
        config.apiKey.trim(),
        false,
        undefined,
        undefined,
        {
          apiFormat:
            config.kind === "custom" ? config.api : "anthropic-messages",
        },
      );
      const existing = new Set(models.map((model) => model.id.trim()));
      const additions = fetched
        .filter((model) => model.id.trim() && !existing.has(model.id.trim()))
        .map((model) => ({
          key: nextModelKey(),
          id: model.id.trim(),
          name: "",
          contextWindow: "",
          maxTokens: "",
        }));
      if (additions.length === 0) {
        toast.info(t("dsh.form.noNewModels"));
        return;
      }
      setConfig((current) => ({
        ...current,
        models: [...current.models, ...additions],
      }));
      toast.success(
        t("dsh.form.fetchModelsSuccess", { count: additions.length }),
      );
    } catch {
      toast.error(t("dsh.form.fetchModelsFailed"));
    } finally {
      setFetchingModels(false);
    }
  };

  const buildSettingsConfig = (displayName: string) => {
    const settings: Record<string, unknown> = {
      kind: config.kind,
      displayName: displayName.trim(),
      baseURL: config.baseURL.trim().replace(/\/+$/, ""),
      reasoningEffort: config.reasoningEffort,
      models: models.map((model) => {
        const contextWindow = Number.parseInt(model.contextWindow, 10);
        const maxTokens = Number.parseInt(model.maxTokens, 10);
        return {
          id: model.id.trim(),
          ...(model.name.trim() ? { name: model.name.trim() } : {}),
          ...(Number.isFinite(contextWindow) && contextWindow > 0
            ? { contextWindow }
            : {}),
          ...(Number.isFinite(maxTokens) && maxTokens > 0 ? { maxTokens } : {}),
        };
      }),
    };
    if (config.kind === "custom") {
      settings.route = config.route.trim();
      settings.api = config.api;
    }
    if (config.apiKey.trim()) {
      settings.apiKey = config.apiKey.trim();
    }
    if (config.apiKeyEnv) {
      settings.apiKeyEnv = config.apiKeyEnv;
    }
    return settings;
  };

  return (
    <Form {...form}>
      <form
        id="provider-form"
        className="space-y-6"
        onSubmit={form.handleSubmit(async (identity) => {
          if (!ready || busy) return;
          setBusy(true);
          onSubmittingChange?.(true);
          setError("");
          try {
            await onSubmit({
              ...identity,
              name: identity.name.trim(),
              meta: initialData?.meta,
              presetCategory: initialData?.category,
              settingsConfig: JSON.stringify(
                buildSettingsConfig(identity.name),
              ),
            });
          } catch (submitError) {
            setError(String(submitError));
          } finally {
            setBusy(false);
            onSubmittingChange?.(false);
          }
        })}
      >
        {!initialData && (
          <ProviderPresetSelector
            selectedPresetId={presetId}
            presetEntries={PRESET_ENTRIES}
            onPresetChange={choosePreset}
          />
        )}

        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}

        <fieldset
          disabled={busy}
          className="min-w-0 space-y-6 border-0 p-0 disabled:opacity-50"
        >
          <BasicFormFields form={form} />

          <p className="text-xs text-muted-foreground">
            {t("dsh.form.storageHint")}
          </p>

          <div className="space-y-2">
            <Label htmlFor="dsh-kind">{t("dsh.form.kind")}</Label>
            <Select
              value={config.kind}
              onValueChange={(value) =>
                update({ kind: value as DshConfigDraft["kind"] })
              }
            >
              <SelectTrigger id="dsh-kind">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="custom">
                  {t("dsh.form.kindCustom")}
                </SelectItem>
                <SelectItem value="deepseek-official">
                  {t("dsh.form.kindOfficial")}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>

          {config.kind === "custom" && (
            <div className="space-y-2">
              <Label
                htmlFor="dsh-route"
                className="after:ms-0.5 after:text-danger-text after:content-['*']"
              >
                {t("dsh.form.route")}
              </Label>
              <Input
                id="dsh-route"
                value={config.route}
                disabled={isEdit}
                onChange={(event) => {
                  setRouteTouched(true);
                  update({ route: event.target.value });
                }}
                placeholder="my-gateway"
                aria-invalid={routeInvalid || undefined}
              />
              <p
                className={
                  routeInvalid
                    ? "text-xs text-danger-text"
                    : "text-xs text-muted-foreground"
                }
              >
                {routeInvalid
                  ? t("dsh.form.routeInvalid")
                  : t("dsh.form.routeHint")}
              </p>
            </div>
          )}

          {config.kind === "custom" && (
            <div className="space-y-2">
              <Label htmlFor="dsh-api">{t("dsh.form.api")}</Label>
              <Select
                value={config.api}
                onValueChange={(value) =>
                  update({ api: value as DshApiFormat })
                }
              >
                <SelectTrigger id="dsh-api">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {DSH_API_FORMATS.map((format) => (
                    <SelectItem key={format.value} value={format.value}>
                      {format.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                {t("dsh.form.apiHint")}
              </p>
            </div>
          )}

          <div className="space-y-2">
            <Label
              htmlFor="dsh-base-url"
              className="after:ms-0.5 after:text-danger-text after:content-['*']"
            >
              {t("dsh.form.baseUrl")}
            </Label>
            <Input
              id="dsh-base-url"
              value={config.baseURL}
              onChange={(event) => update({ baseURL: event.target.value })}
              placeholder={t("dsh.form.baseUrlPlaceholder")}
              aria-invalid={baseUrlInvalid || undefined}
            />
            <p
              className={
                baseUrlInvalid
                  ? "text-xs text-danger-text"
                  : "text-xs text-muted-foreground"
              }
            >
              {baseUrlInvalid
                ? t("dsh.form.baseUrlInvalid")
                : t("dsh.form.baseUrlHint")}
            </p>
          </div>

          <ApiKeyInput
            id="dsh-api-key"
            value={config.apiKey}
            onChange={(value) => update({ apiKey: value })}
            label={t("dsh.form.apiKey")}
            hint={t("dsh.form.apiKeyHint")}
          />

          <div className="space-y-2">
            <div className="flex items-center justify-between gap-2">
              <Label>{t("dsh.form.models")}</Label>
              <div className="flex items-center gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={fetchingModels || !config.baseURL.trim()}
                  onClick={() => void fetchModels()}
                >
                  <RefreshCw
                    className={`me-1 h-4 w-4 ${fetchingModels ? "animate-spin" : ""}`}
                  />
                  {fetchingModels
                    ? t("dsh.form.fetchingModels")
                    : t("dsh.form.fetchModels")}
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    update({ models: [...config.models, emptyModel()] })
                  }
                >
                  <Plus className="me-1 h-4 w-4" />
                  {t("dsh.form.addModel")}
                </Button>
              </div>
            </div>
            {config.models.length === 0 && (
              <p className="text-xs text-muted-foreground">
                {t("dsh.form.modelsEmpty")}
              </p>
            )}
            <div className="space-y-3">
              {config.models.map((model, index) => (
                <div
                  key={model.key}
                  className="space-y-2 rounded-lg border border-border p-3"
                >
                  <div className="flex items-center justify-between">
                    <span className="text-xs text-muted-foreground">
                      {t("dsh.form.modelIndex", { index: index + 1 })}
                    </span>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      aria-label={t("dsh.form.removeModel")}
                      onClick={() =>
                        update({
                          models: config.models.filter(
                            (item) => item.key !== model.key,
                          ),
                        })
                      }
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                  <Input
                    value={model.id}
                    onChange={(event) =>
                      updateModel(model.key, { id: event.target.value })
                    }
                    placeholder={t("dsh.form.modelId")}
                    aria-label={t("dsh.form.modelId")}
                  />
                  <div className="grid gap-2 sm:grid-cols-2">
                    <Input
                      value={model.name}
                      onChange={(event) =>
                        updateModel(model.key, { name: event.target.value })
                      }
                      placeholder={t("dsh.form.modelName")}
                      aria-label={t("dsh.form.modelName")}
                    />
                    <Input
                      value={model.contextWindow}
                      inputMode="numeric"
                      onChange={(event) =>
                        updateModel(model.key, {
                          contextWindow: event.target.value.replace(/\D/g, ""),
                        })
                      }
                      placeholder={t("dsh.form.contextWindow")}
                      aria-label={t("dsh.form.contextWindow")}
                    />
                    <Input
                      value={model.maxTokens}
                      inputMode="numeric"
                      onChange={(event) =>
                        updateModel(model.key, {
                          maxTokens: event.target.value.replace(/\D/g, ""),
                        })
                      }
                      placeholder={t("dsh.form.maxTokens")}
                      aria-label={t("dsh.form.maxTokens")}
                    />
                  </div>
                </div>
              ))}
            </div>
          </div>

          <div className="space-y-2">
            <Label htmlFor="dsh-effort">{t("dsh.form.reasoningEffort")}</Label>
            <Select
              value={config.reasoningEffort}
              onValueChange={(value) =>
                update({ reasoningEffort: value as DshReasoningEffort })
              }
            >
              <SelectTrigger id="dsh-effort">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {DSH_REASONING_EFFORTS.map((effort) => (
                  <SelectItem key={effort} value={effort}>
                    {effort}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground">
              {t("dsh.form.reasoningEffortHint")}
            </p>
          </div>
        </fieldset>

        {showButtons && (
          <div className="flex justify-end gap-2">
            <Button type="button" variant="outline" onClick={onCancel}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={!ready || busy}>
              {busy ? t("common.saving") : submitLabel}
            </Button>
          </div>
        )}
      </form>
    </Form>
  );
}
