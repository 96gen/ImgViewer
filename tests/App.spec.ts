import { mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
const bridgeMocks = vi.hoisted(() => {
  const initial = {
    generation: 5,
    status: "error" as const,
    index: 1,
    total: 3,
    fileName: "broken.jpg",
    canPrevious: true,
    canNext: true,
    error: { code: "corrupt", message: "圖片資料已損壞" },
  };
  let current: Record<string, unknown> = initial;
  const currentSnapshot = vi.fn(async () => current);
  const setCurrentSnapshot = (next: Record<string, unknown>) => {
    current = next;
  };
  const navigate = vi.fn(async (direction: "previous" | "next") => {
    const { error: _discardedError, ...withoutError } = initial;
    return {
      ...withoutError,
      generation: direction === "next" ? 6 : 7,
      status: "loading" as const,
      fileName: direction === "next" ? "next.jpg" : "previous.jpg",
    };
  });
  const chooseImage = vi.fn(async () => "C:\\images\\picked.png");
  const openPath = vi.fn(async () => {
    const { error: _discardedError, ...withoutError } = initial;
    return { ...withoutError, generation: 8, status: "loading" as const };
  });
  const readRender = vi.fn(async () => new Uint8Array([1, 2, 3]));
  return {
    initial,
    navigate,
    chooseImage,
    openPath,
    currentSnapshot,
    setCurrentSnapshot,
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
      listenSnapshot: vi.fn(async () => vi.fn()),
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
});
