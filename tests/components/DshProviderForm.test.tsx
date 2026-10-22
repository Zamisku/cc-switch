import { fireEvent, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const fetchModelsForConfig = vi.fn();
vi.mock("@/lib/api/model-fetch", () => ({
  fetchModelsForConfig: (...args: unknown[]) => fetchModelsForConfig(...args),
}));

import { DshProviderForm } from "@/components/providers/forms/DshProviderForm";
import { renderWithQueryClient as render } from "../utils/testQueryClient";

function renderForm(onSubmit = vi.fn(), initialData?: Record<string, unknown>) {
  return render(
    <DshProviderForm
      appId="dsh"
      submitLabel="Save DSH provider"
      onSubmit={onSubmit}
      onCancel={() => {}}
      initialData={initialData}
    />,
  );
}

describe("DshProviderForm", () => {
  beforeEach(() => {
    Element.prototype.scrollIntoView = vi.fn();
  });

  it("submits the official DeepSeek route as DSH settings JSON", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    renderForm(onSubmit);

    await user.click(
      screen.getByRole("button", { name: "dsh.preset.official" }),
    );
    await user.click(screen.getByRole("button", { name: "Save DSH provider" }));

    expect(onSubmit).toHaveBeenCalledTimes(1);
    const values = onSubmit.mock.calls[0][0];
    const settings = JSON.parse(values.settingsConfig);
    expect(settings).toMatchObject({
      kind: "deepseek-official",
      baseURL: "https://api.deepseek.com/anthropic",
      reasoningEffort: "high",
    });
    expect(settings.route).toBeUndefined();
    expect(settings.api).toBeUndefined();
    expect(settings.models.length).toBeGreaterThan(0);
    expect(settings.models[0].id).toBe("deepseek-chat");
  });

  it("keeps custom routes blocked until they have an endpoint and a model", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    renderForm(onSubmit);

    await user.click(
      screen.getByRole("button", { name: "providerPreset.custom" }),
    );
    const save = screen.getByRole("button", { name: "Save DSH provider" });
    expect(save).toBeDisabled();

    await user.type(screen.getByLabelText("provider.name"), "Local Mock");
    await user.type(
      document.querySelector("#dsh-base-url") as HTMLInputElement,
      "http://127.0.0.1:8787",
    );
    expect(save).toBeDisabled();

    await user.type(screen.getByLabelText("dsh.form.modelId"), "mock-sonnet");
    await waitFor(() => expect(save).toBeEnabled());

    await user.click(save);
    const settings = JSON.parse(onSubmit.mock.calls[0][0].settingsConfig);
    expect(settings).toMatchObject({
      kind: "custom",
      route: "local-mock",
      api: "anthropic-messages",
      baseURL: "http://127.0.0.1:8787",
      models: [{ id: "mock-sonnet" }],
    });
    expect(settings.apiKey).toBeUndefined();
  });

  it("mirrors an existing route and disables renaming it", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    renderForm(onSubmit, {
      name: "Gateway",
      settingsConfig: {
        kind: "custom",
        route: "my-gateway",
        displayName: "Gateway",
        api: "openai-completions",
        baseURL: "https://gateway.example.com",
        apiKey: "sk-local",
        models: [{ id: "gpt-5", name: "GPT-5" }],
        reasoningEffort: "low",
      },
    });

    const route = document.querySelector("#dsh-route") as HTMLInputElement;
    expect(route.value).toBe("my-gateway");
    expect(route).toBeDisabled();

    await user.click(screen.getByRole("button", { name: "Save DSH provider" }));
    const settings = JSON.parse(onSubmit.mock.calls[0][0].settingsConfig);
    expect(settings).toMatchObject({
      kind: "custom",
      route: "my-gateway",
      api: "openai-completions",
      baseURL: "https://gateway.example.com",
      apiKey: "sk-local",
      reasoningEffort: "low",
      models: [{ id: "gpt-5", name: "GPT-5" }],
    });
  });

  it("fetches models from the endpoint and adds new ids", async () => {
    const user = userEvent.setup();
    fetchModelsForConfig.mockReset().mockResolvedValue([
      { id: "mock-sonnet", ownedBy: null },
      { id: "mock-haiku", ownedBy: null },
    ]);
    renderForm(vi.fn(), {
      name: "Local",
      settingsConfig: {
        kind: "custom",
        route: "local",
        api: "anthropic-messages",
        baseURL: "http://127.0.0.1:8787",
        apiKey: "sk-test",
        models: [{ id: "mock-sonnet" }],
      },
    });

    await user.click(
      screen.getByRole("button", { name: "dsh.form.fetchModels" }),
    );

    expect(fetchModelsForConfig).toHaveBeenCalledWith(
      "http://127.0.0.1:8787",
      "sk-test",
      false,
      undefined,
      undefined,
      { apiFormat: "anthropic-messages" },
    );
    await waitFor(() =>
      expect(screen.getAllByLabelText("dsh.form.modelId")).toHaveLength(2),
    );
    expect(
      (screen.getAllByLabelText("dsh.form.modelId")[1] as HTMLInputElement)
        .value,
    ).toBe("mock-haiku");
  });

  it("rejects a route that DSH would not accept", async () => {
    const user = userEvent.setup();
    const onSubmit = vi.fn();
    renderForm(onSubmit, {
      name: "Bad",
      settingsConfig: {
        kind: "custom",
        route: "Bad Route",
        api: "anthropic-messages",
        baseURL: "https://example.com",
        models: [{ id: "m" }],
      },
    });

    const save = screen.getByRole("button", { name: "Save DSH provider" });
    expect(screen.getByText("dsh.form.routeInvalid")).toBeInTheDocument();
    expect(save).toBeDisabled();

    fireEvent.change(document.querySelector("#dsh-route") as Element, {
      target: { value: "bad-route" },
    });
    await waitFor(() => expect(save).toBeEnabled());
    expect(screen.queryByText("dsh.form.routeInvalid")).not.toBeInTheDocument();

    await user.click(save);
    const settings = JSON.parse(onSubmit.mock.calls[0][0].settingsConfig);
    expect(settings.route).toBe("bad-route");
  });
});
