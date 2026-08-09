import { mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
const bridgeMocks = vi.hoisted(() => {
  const initial = {
    protocolVersion: 1,
    revision: 5,
    generation: 5,
    status: "error" as const,
    index: 1,
    total: 3,
    fileName: "broken.jpg",
    canPrevious: true,
    canNext: true,
    error: { code: "corrupt", message: "圖片資料已損壞", parameters: {} },
  };
  let current: Record<string, unknown> = initial;
  const currentSnapshot = vi.fn(async () => current);
  let snapshotListener: ((snapshot: Record<string, unknown>) => void) | null =
    null;
  const setCurrentSnapshot = (next: Record<string, unknown>) => {
    current = next;
  };
  const emitSnapshot = (next: Record<string, unknown>) => {
    snapshotListener?.(next);
  };
  const listenSnapshot = vi.fn(
    async (listener: (snapshot: Record<string, unknown>) => void) => {
      snapshotListener = listener;
      return vi.fn(() => {
        snapshotListener = null;
      });
    },
  );
  const navigate = vi.fn(async (direction: "previous" | "next") => {
    const { error: _discardedError, ...withoutError } = initial;
    return {
      ...withoutError,
      revision: direction === "next" ? 100 : 101,
      generation: direction === "next" ? 6 : 7,
      status: "loading" as const,
      fileName: direction === "next" ? "next.jpg" : "previous.jpg",
    };
  });
  const chooseImage = vi.fn(async () => "C:\\images\\picked.png");
  const openPath = vi.fn(async () => {
    const { error: _discardedError, ...withoutError } = initial;
    return {
      ...withoutError,
      revision: 102,
      generation: 8,
      status: "loading" as const,
    };
  });
  const readRender = vi.fn(async () => new Uint8Array([1, 2, 3]));
  return {
    initial,
    navigate,
    chooseImage,
    openPath,
    currentSnapshot,
    setCurrentSnapshot,
    emitSnapshot,
    listenSnapshot,
    readRender,
  };
});

vi.mock("../src/services/viewerBridge", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/services/viewerBridge")>();
  return {
    ...original,
    tauriViewerBridge: {
      chooseImage: bridgeMocks.chooseImage,
      openPath: bridgeMocks.openPath,
      navigate: bridgeMocks.navigate,
      currentSnapshot: bridgeMocks.currentSnapshot,
      readRender: bridgeMocks.readRender,
      listenSnapshot: bridgeMocks.listenSnapshot,
      listenFileDrop: vi.fn(async () => vi.fn()),
    },
  };
});

import App from "../src/App.vue";

describe("App keyboard and navigation", () => {
  afterEach(() => {
    bridgeMocks.setCurrentSnapshot(bridgeMocks.initial);
    bridgeMocks.navigate.mockClear();
    bridgeMocks.chooseImage.mockClear();
    bridgeMocks.openPath.mockClear();
    bridgeMocks.currentSnapshot.mockClear();
    bridgeMocks.readRender.mockClear();
    bridgeMocks.listenSnapshot.mockClear();
  });

  it("uses arrow keys and both visible navigation buttons", async () => {
    const wrapper = mount(App, { attachTo: document.body });
    await vi.waitFor(() => expect(wrapper.text()).toContain("broken.jpg"));

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight" }));
    await vi.waitFor(() => expect(bridgeMocks.navigate).toHaveBeenCalledWith("next"));

    await wrapper.get('button[aria-label="上一張"]').trigger("click");
    expect(bridgeMocks.navigate).toHaveBeenCalledWith("previous");

    await wrapper.get('button[aria-label="下一張"]').trigger("click");
    expect(bridgeMocks.navigate).toHaveBeenCalledWith("next");
    wrapper.unmount();
  });

  it("delivers a rapid pair of ArrowRight events before either invoke resolves", async () => {
    let resolveFirst!: (snapshot: Record<string, unknown>) => void;
    let resolveSecond!: (snapshot: Record<string, unknown>) => void;
    const first = new Promise<Record<string, unknown>>((resolve) => {
      resolveFirst = resolve;
    });
    const second = new Promise<Record<string, unknown>>((resolve) => {
      resolveSecond = resolve;
    });
    bridgeMocks.navigate
      .mockImplementationOnce(() => first)
      .mockImplementationOnce(() => second);

    const wrapper = mount(App, { attachTo: document.body });
    await vi.waitFor(() => expect(wrapper.text()).toContain("broken.jpg"));

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight" }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight" }));
    expect(bridgeMocks.navigate).toHaveBeenCalledTimes(2);
    expect(bridgeMocks.navigate.mock.calls).toEqual([["next"], ["next"]]);

    const { error: _discardedError, ...base } = bridgeMocks.initial;
    resolveSecond({
      ...base,
      revision: 102,
      generation: 7,
      status: "loading",
      fileName: "rapid-final.jpg",
    });
    await vi.waitFor(() => expect(wrapper.text()).toContain("rapid-final.jpg"));

    resolveFirst({
      ...base,
      revision: 101,
      generation: 6,
      status: "loading",
      fileName: "rapid-stale.jpg",
    });
    await Promise.resolve();
    expect(wrapper.text()).toContain("rapid-final.jpg");
    expect(wrapper.text()).not.toContain("rapid-stale.jpg");
    wrapper.unmount();
  });

  it("opens the dialog with Ctrl+O", async () => {
    const wrapper = mount(App, { attachTo: document.body });
    await vi.waitFor(() => expect(wrapper.text()).toContain("broken.jpg"));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "o", ctrlKey: true }));

    await vi.waitFor(() => expect(bridgeMocks.chooseImage).toHaveBeenCalledOnce());
    expect(bridgeMocks.openPath).toHaveBeenCalledWith("C:\\images\\picked.png");
    wrapper.unmount();
  });

  it("keeps navigation enabled after an inline decode error", async () => {
    const wrapper = mount(App, { attachTo: document.body });
    await vi.waitFor(() => expect(wrapper.text()).toContain("圖片資料已損壞"));

    const next = wrapper.get('button[aria-label="下一張"]');
    expect(next.attributes("disabled")).toBeUndefined();
    await next.trigger("click");
    expect(bridgeMocks.navigate).toHaveBeenCalledWith("next");
    wrapper.unmount();
  });

  it("keeps the committed viewport mounted while the next image is loading", async () => {
    const decodeDescriptor = Object.getOwnPropertyDescriptor(
      HTMLImageElement.prototype,
      "decode",
    );
    Object.defineProperty(HTMLImageElement.prototype, "decode", {
      configurable: true,
      value: vi.fn(async () => undefined),
    });
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:committed");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    bridgeMocks.setCurrentSnapshot({
      protocolVersion: 1,
      revision: 10,
      generation: 5,
      status: "ready",
      index: 1,
      total: 3,
      fileName: "current.png",
      canPrevious: true,
      canNext: true,
      render: {
        renderId: 50,
        mimeType: "image/png",
        width: 640,
        height: 480,
        animated: false,
      },
    });

    const wrapper = mount(App, { attachTo: document.body });
    await vi.waitFor(() =>
      expect(wrapper.get("img").attributes("src")).toBe("blob:committed"),
    );
    const viewport = wrapper.get('[data-testid="image-viewport"]').element;

    await wrapper.get('button[aria-label="下一張"]').trigger("click");
    await vi.waitFor(() => expect(wrapper.text()).toContain("next.jpg"));

    expect(wrapper.get('[data-testid="image-viewport"]').element).toBe(viewport);
    expect(wrapper.get("img").attributes("src")).toBe("blob:committed");
    expect(wrapper.find(".state-panel.loading-state").exists()).toBe(false);
    expect(wrapper.find(".empty-state").exists()).toBe(false);
    expect(wrapper.find(".switching-indicator").exists()).toBe(true);

    wrapper.unmount();
    if (decodeDescriptor) {
      Object.defineProperty(
        HTMLImageElement.prototype,
        "decode",
        decodeDescriptor,
      );
    } else {
      Reflect.deleteProperty(HTMLImageElement.prototype, "decode");
    }
  });

  it("shows command failures over the committed image instead of hiding them", async () => {
    const decodeDescriptor = Object.getOwnPropertyDescriptor(
      HTMLImageElement.prototype,
      "decode",
    );
    Object.defineProperty(HTMLImageElement.prototype, "decode", {
      configurable: true,
      value: vi.fn(async () => undefined),
    });
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:committed-error");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    bridgeMocks.setCurrentSnapshot({
      protocolVersion: 1,
      revision: 20,
      generation: 9,
      status: "ready",
      index: 1,
      total: 3,
      fileName: "current.png",
      canPrevious: true,
      canNext: true,
      render: {
        renderId: 90,
        mimeType: "image/png",
        width: 640,
        height: 480,
        animated: false,
      },
    });
    bridgeMocks.navigate.mockRejectedValueOnce(new Error("後端協定驗證失敗"));

    const wrapper = mount(App, { attachTo: document.body });
    await vi.waitFor(() =>
      expect(wrapper.get("img").attributes("src")).toBe("blob:committed-error"),
    );
    await wrapper.get('button[aria-label="下一張"]').trigger("click");

    await vi.waitFor(() =>
      expect(wrapper.get(".client-error-banner").text()).toContain(
        "後端協定驗證失敗",
      ),
    );
    expect(wrapper.get("img").attributes("src")).toBe("blob:committed-error");
    await wrapper.get('button[aria-label="關閉錯誤提示"]').trigger("click");
    expect(wrapper.find(".client-error-banner").exists()).toBe(false);

    wrapper.unmount();
    if (decodeDescriptor) {
      Object.defineProperty(
        HTMLImageElement.prototype,
        "decode",
        decodeDescriptor,
      );
    } else {
      Reflect.deleteProperty(HTMLImageElement.prototype, "decode");
    }
  });

  it("does not restore the switching spinner when a render failure is dismissed", async () => {
    const decodeDescriptor = Object.getOwnPropertyDescriptor(
      HTMLImageElement.prototype,
      "decode",
    );
    Object.defineProperty(HTMLImageElement.prototype, "decode", {
      configurable: true,
      value: vi.fn(async () => undefined),
    });
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:committed-render-error");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    bridgeMocks.setCurrentSnapshot({
      protocolVersion: 1,
      revision: 20,
      generation: 5,
      status: "ready",
      index: 1,
      total: 3,
      fileName: "current.png",
      canPrevious: true,
      canNext: true,
      render: {
        renderId: 90,
        mimeType: "image/png",
        width: 640,
        height: 480,
        animated: false,
      },
    });

    const wrapper = mount(App, { attachTo: document.body });
    await vi.waitFor(() =>
      expect(wrapper.get("img").attributes("src")).toBe(
        "blob:committed-render-error",
      ),
    );
    await wrapper.get('button[aria-label="下一張"]').trigger("click");
    await vi.waitFor(() =>
      expect(wrapper.find(".switching-indicator").exists()).toBe(true),
    );

    bridgeMocks.readRender.mockRejectedValueOnce(new Error("render token 已失效"));
    bridgeMocks.emitSnapshot({
      protocolVersion: 1,
      revision: 101,
      generation: 6,
      status: "ready",
      index: 1,
      total: 3,
      fileName: "next.jpg",
      canPrevious: true,
      canNext: true,
      render: {
        renderId: 91,
        mimeType: "image/jpeg",
        width: 800,
        height: 600,
        animated: false,
      },
    });

    await vi.waitFor(() =>
      expect(wrapper.get(".client-error-banner").text()).toContain(
        "render token 已失效",
      ),
    );
    expect(wrapper.get("img").attributes("src")).toBe(
      "blob:committed-render-error",
    );
    expect(wrapper.find(".switching-indicator").exists()).toBe(false);

    await wrapper.get('button[aria-label="關閉錯誤提示"]').trigger("click");
    expect(wrapper.find(".client-error-banner").exists()).toBe(false);
    expect(wrapper.find(".switching-indicator").exists()).toBe(false);

    wrapper.unmount();
    if (decodeDescriptor) {
      Object.defineProperty(
        HTMLImageElement.prototype,
        "decode",
        decodeDescriptor,
      );
    } else {
      Reflect.deleteProperty(HTMLImageElement.prototype, "decode");
    }
  });

  it("shows an offline About dialog with a selectable release URL", async () => {
    const wrapper = mount(App, { attachTo: document.body });
    await vi.waitFor(() => expect(wrapper.text()).toContain("broken.jpg"));

    await wrapper.get('button[aria-label="關於 ImgViewer"]').trigger("click");
    const dialog = wrapper.get('[role="dialog"]');
    expect(dialog.text()).toContain("ImgViewer 0.4.0");
    expect(dialog.text()).toContain("不含遙測");
    expect(dialog.get("input").attributes("value")).toBe(
      "https://github.com/96gen/ImgViewer/releases",
    );

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    await wrapper.vm.$nextTick();
    expect(wrapper.find('[role="dialog"]').exists()).toBe(false);
    wrapper.unmount();
  });
});
