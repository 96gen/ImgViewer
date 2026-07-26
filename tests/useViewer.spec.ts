import { defineComponent, nextTick, watch } from "vue";
import { mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import { useViewer } from "../src/composables/useViewer";
import type { ImagePreloader } from "../src/composables/useViewer";
import type { ViewerBridge } from "../src/services/viewerBridge";
import type { ViewerSnapshot } from "../src/types/viewer";

const emptySnapshot = (generation = 0): ViewerSnapshot => ({
  protocolVersion: 1,
  revision: generation * 2,
  generation,
  status: "empty",
  index: null,
  total: 0,
  fileName: null,
  canPrevious: false,
  canNext: false,
});

const readySnapshot = (generation: number, renderId: number): ViewerSnapshot => ({
  protocolVersion: 1,
  revision: generation * 2 + 1,
  generation,
  status: "ready",
  index: generation,
  total: 10,
  fileName: `${generation}.png`,
  canPrevious: generation > 0,
  canNext: true,
  render: {
    renderId,
    mimeType: "image/png",
    width: 100,
    height: 80,
    animated: false,
  },
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function fakeBridge(overrides: Partial<ViewerBridge> = {}): ViewerBridge {
  return {
    chooseImage: vi.fn(async () => null),
    openPath: vi.fn(async () => emptySnapshot()),
    navigate: vi.fn(async () => emptySnapshot()),
    currentSnapshot: vi.fn(async () => emptySnapshot()),
    readRender: vi.fn(async () => new Uint8Array([1, 2, 3])),
    listenSnapshot: vi.fn(async () => vi.fn()),
    listenFileDrop: vi.fn(async () => vi.fn()),
    ...overrides,
  };
}

const immediatePreloader: ImagePreloader = () => ({
  ready: Promise.resolve(),
  release: vi.fn(),
});

function mountSession(
  bridge: ViewerBridge,
  preloader: ImagePreloader = immediatePreloader,
) {
  let session!: ReturnType<typeof useViewer>;
  const wrapper = mount(
    defineComponent({
      setup() {
        session = useViewer(bridge, preloader);
        return () => null;
      },
    }),
  );
  return { wrapper, session };
}

describe("useViewer", () => {
  it("does not regress from ready to a late loading response in the same generation", async () => {
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:ready");
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    const { wrapper, session } = mountSession(fakeBridge());

    await session.applySnapshot(readySnapshot(7, 70));
    await session.applySnapshot({
      protocolVersion: 1,
      revision: 14,
      generation: 7,
      status: "loading",
      index: 7,
      total: 10,
      fileName: "7.png",
      canPrevious: true,
      canNext: true,
    });

    expect(session.snapshot.value?.status).toBe("ready");
    expect(session.imageUrl.value).toBe("blob:ready");
    expect(revoke).not.toHaveBeenCalledWith("blob:ready");
    wrapper.unmount();
  });

  it("keeps the committed frame while the next generation is loading", async () => {
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:old");
    const revoke = vi
      .spyOn(URL, "revokeObjectURL")
      .mockImplementation(() => undefined);
    const { wrapper, session } = mountSession(fakeBridge());

    await session.applySnapshot(readySnapshot(1, 11));
    await session.applySnapshot({
      protocolVersion: 1,
      revision: 4,
      generation: 2,
      status: "loading",
      index: 2,
      total: 10,
      fileName: "2.png",
      canPrevious: true,
      canNext: true,
    });

    expect(session.snapshot.value?.status).toBe("loading");
    expect(session.snapshot.value?.generation).toBe(2);
    expect(session.displayedImage.value?.generation).toBe(1);
    expect(session.imageUrl.value).toBe("blob:old");
    expect(revoke).not.toHaveBeenCalledWith("blob:old");
    wrapper.unmount();
  });

  it("swaps only after the candidate image preload completes", async () => {
    const nextReady = deferred<void>();
    let serial = 0;
    const create = vi
      .spyOn(URL, "createObjectURL")
      .mockImplementation(() => `blob:${++serial}`);
    const revoke = vi
      .spyOn(URL, "revokeObjectURL")
      .mockImplementation(() => undefined);
    const releases = new Map<string, ReturnType<typeof vi.fn>>();
    const preloader: ImagePreloader = (url) => {
      const release = vi.fn();
      releases.set(url, release);
      return {
        ready: url === "blob:2" ? nextReady.promise : Promise.resolve(),
        release,
      };
    };
    const { wrapper, session } = mountSession(fakeBridge(), preloader);

    await session.applySnapshot(readySnapshot(1, 11));
    const nextLoad = session.applySnapshot(readySnapshot(2, 12));
    await vi.waitFor(() => expect(create).toHaveBeenCalledTimes(2));

    expect(session.snapshot.value?.generation).toBe(2);
    expect(session.displayedImage.value?.generation).toBe(1);
    expect(session.imageUrl.value).toBe("blob:1");
    expect(session.renderPending.value).toBe(true);
    expect(revoke).not.toHaveBeenCalledWith("blob:1");
    expect(revoke).not.toHaveBeenCalledWith("blob:2");

    nextReady.resolve(undefined);
    await nextLoad;

    expect(session.displayedImage.value?.generation).toBe(2);
    expect(session.imageUrl.value).toBe("blob:2");
    expect(session.renderPending.value).toBe(false);
    expect(revoke).toHaveBeenCalledTimes(1);
    expect(revoke).toHaveBeenCalledWith("blob:1");
    expect(releases.get("blob:2")).toHaveBeenCalledOnce();
    wrapper.unmount();
  });

  it("registers the snapshot listener before reading startup state", async () => {
    const order: string[] = [];
    const bridge = fakeBridge({
      listenSnapshot: vi.fn(async () => {
        order.push("listen");
        return vi.fn();
      }),
      currentSnapshot: vi.fn(async () => {
        order.push("current");
        return emptySnapshot();
      }),
    });
    const { wrapper } = mountSession(bridge);

    await nextTick();
    await vi.waitFor(() => expect(order).toContain("current"));
    expect(order.slice(0, 2)).toEqual(["listen", "current"]);
    wrapper.unmount();
  });

  it("ignores an old render response after a newer generation arrives", async () => {
    const oldBytes = deferred<ArrayBuffer>();
    const bridge = fakeBridge({
      readRender: vi.fn((renderId) =>
        renderId === 1 ? oldBytes.promise : Promise.resolve(new Uint8Array([2])),
      ),
    });
    const createUrl = vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:new");
    const { wrapper, session } = mountSession(bridge);

    const oldLoad = session.applySnapshot(readySnapshot(1, 1));
    const { render: _oldRender, ...withoutRender } = readySnapshot(2, 2);
    await session.applySnapshot({
      ...withoutRender,
      status: "error",
      error: { code: "broken", message: "檔案已損壞", parameters: {} },
    });
    oldBytes.resolve(new Uint8Array([1]).buffer);
    await oldLoad;

    expect(session.snapshot.value?.generation).toBe(2);
    expect(session.imageUrl.value).toBeNull();
    expect(createUrl).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it("rechecks generation and revision after URL creation before installing a stale preload", async () => {
    const readRender = vi.fn(async (renderId: number) =>
      new Uint8Array([renderId]).buffer,
    );
    const staleReady = deferred<void>();
    const staleRelease = vi.fn();
    const latestRelease = vi.fn();
    const preloader = vi.fn<ImagePreloader>((url) =>
      url === "blob:stale"
        ? { ready: staleReady.promise, release: staleRelease }
        : { ready: Promise.resolve(), release: latestRelease },
    );
    const revoke = vi
      .spyOn(URL, "revokeObjectURL")
      .mockImplementation(() => undefined);
    let switchToLatest: Promise<void> | null = null;
    let session!: ReturnType<typeof useViewer>;
    vi.spyOn(URL, "createObjectURL")
      .mockImplementationOnce(() => {
        switchToLatest = session.applySnapshot(readySnapshot(2, 2));
        return "blob:stale";
      })
      .mockReturnValueOnce("blob:latest");
    const mounted = mountSession(fakeBridge({ readRender }), preloader);
    session = mounted.session;

    await session.applySnapshot(readySnapshot(1, 1));
    if (!switchToLatest) throw new Error("expected a generation switch");
    await switchToLatest;

    expect(readRender.mock.calls.map(([renderId]) => renderId)).toEqual([1, 2]);
    expect(preloader).toHaveBeenCalledTimes(1);
    expect(preloader).toHaveBeenCalledWith("blob:latest");
    expect(staleRelease).not.toHaveBeenCalled();
    expect(session.displayedImage.value?.generation).toBe(2);
    expect(session.imageUrl.value).toBe("blob:latest");
    expect(revoke.mock.calls.filter(([url]) => url === "blob:stale")).toHaveLength(
      1,
    );

    mounted.wrapper.unmount();
    expect(revoke.mock.calls.filter(([url]) => url === "blob:latest")).toHaveLength(
      1,
    );
  });

  it("never displays a stale preload and revokes its candidate URL", async () => {
    const staleReady = deferred<void>();
    let serial = 0;
    const create = vi
      .spyOn(URL, "createObjectURL")
      .mockImplementation(() => `blob:${++serial}`);
    const revoke = vi
      .spyOn(URL, "revokeObjectURL")
      .mockImplementation(() => undefined);
    const releases = new Map<string, ReturnType<typeof vi.fn>>();
    const preloader: ImagePreloader = (url) => {
      const release = vi.fn();
      releases.set(url, release);
      return {
        ready: url === "blob:2" ? staleReady.promise : Promise.resolve(),
        release,
      };
    };
    const { wrapper, session } = mountSession(fakeBridge(), preloader);

    await session.applySnapshot(readySnapshot(1, 11));
    const displayedUrls: string[] = [];
    const stopWatching = watch(
      session.imageUrl,
      (url) => {
        if (url) displayedUrls.push(url);
      },
      { flush: "sync" },
    );

    const staleLoad = session.applySnapshot(readySnapshot(2, 12));
    await vi.waitFor(() => expect(create).toHaveBeenCalledTimes(2));
    const latestLoad = session.applySnapshot(readySnapshot(3, 13));

    await vi.waitFor(() => expect(revoke).toHaveBeenCalledWith("blob:2"));
    expect(session.imageUrl.value).toBe("blob:1");
    await Promise.all([staleLoad, latestLoad]);

    expect(session.displayedImage.value?.generation).toBe(3);
    expect(session.imageUrl.value).toBe("blob:3");
    expect(displayedUrls).not.toContain("blob:2");
    expect(revoke.mock.calls.filter(([url]) => url === "blob:2")).toHaveLength(1);
    expect(releases.get("blob:2")).toHaveBeenCalledOnce();

    staleReady.resolve(undefined);
    await nextTick();
    expect(session.imageUrl.value).toBe("blob:3");

    stopWatching();
    wrapper.unmount();
  });

  it("revokes both the committed and pending URLs when disposed", async () => {
    const pendingReady = deferred<void>();
    let serial = 0;
    const create = vi
      .spyOn(URL, "createObjectURL")
      .mockImplementation(() => `blob:${++serial}`);
    const revoke = vi
      .spyOn(URL, "revokeObjectURL")
      .mockImplementation(() => undefined);
    const releases = new Map<string, ReturnType<typeof vi.fn>>();
    const preloader: ImagePreloader = (url) => {
      const release = vi.fn();
      releases.set(url, release);
      return {
        ready: url === "blob:2" ? pendingReady.promise : Promise.resolve(),
        release,
      };
    };
    const { wrapper, session } = mountSession(fakeBridge(), preloader);

    await session.applySnapshot(readySnapshot(1, 11));
    const pendingLoad = session.applySnapshot(readySnapshot(2, 12));
    await vi.waitFor(() => expect(create).toHaveBeenCalledTimes(2));

    wrapper.unmount();
    await pendingLoad;
    pendingReady.resolve(undefined);
    await nextTick();

    expect(session.imageUrl.value).toBeNull();
    expect(session.renderPending.value).toBe(false);
    expect(revoke.mock.calls.map(([url]) => url).sort()).toEqual([
      "blob:1",
      "blob:2",
    ]);
    expect(releases.get("blob:1")).toHaveBeenCalledOnce();
    expect(releases.get("blob:2")).toHaveBeenCalledOnce();
  });

  it("keeps the committed frame when the next binary read fails", async () => {
    let serial = 0;
    vi.spyOn(URL, "createObjectURL").mockImplementation(
      () => `blob:${++serial}`,
    );
    const revoke = vi
      .spyOn(URL, "revokeObjectURL")
      .mockImplementation(() => undefined);
    const readRender = vi.fn((renderId: number) =>
      renderId === 12
        ? Promise.reject(new Error("render token 已失效"))
        : Promise.resolve(new Uint8Array([renderId]).buffer),
    );
    const { wrapper, session } = mountSession(fakeBridge({ readRender }));

    await session.applySnapshot(readySnapshot(1, 11));
    await session.applySnapshot(readySnapshot(2, 12));

    expect(session.snapshot.value?.generation).toBe(2);
    expect(session.displayedImage.value?.generation).toBe(1);
    expect(session.imageUrl.value).toBe("blob:1");
    expect(session.clientError.value).toContain("render token 已失效");
    expect(session.renderPending.value).toBe(false);
    expect(revoke).not.toHaveBeenCalled();

    wrapper.unmount();
    expect(revoke.mock.calls.filter(([url]) => url === "blob:1")).toHaveLength(1);
  });

  it("keeps the committed frame and revokes only the failed preload candidate", async () => {
    let serial = 0;
    vi.spyOn(URL, "createObjectURL").mockImplementation(
      () => `blob:${++serial}`,
    );
    const revoke = vi
      .spyOn(URL, "revokeObjectURL")
      .mockImplementation(() => undefined);
    const releases = new Map<string, ReturnType<typeof vi.fn>>();
    const preloader: ImagePreloader = (url) => {
      const release = vi.fn();
      releases.set(url, release);
      return {
        ready:
          url === "blob:2"
            ? Promise.reject(new Error("WebView2 預解碼失敗"))
            : Promise.resolve(),
        release,
      };
    };
    const { wrapper, session } = mountSession(fakeBridge(), preloader);

    await session.applySnapshot(readySnapshot(1, 11));
    await session.applySnapshot(readySnapshot(2, 12));

    expect(session.snapshot.value?.generation).toBe(2);
    expect(session.displayedImage.value?.generation).toBe(1);
    expect(session.imageUrl.value).toBe("blob:1");
    expect(session.clientError.value).toContain("WebView2 預解碼失敗");
    expect(session.renderPending.value).toBe(false);
    expect(revoke.mock.calls.filter(([url]) => url === "blob:2")).toHaveLength(1);
    expect(revoke).not.toHaveBeenCalledWith("blob:1");
    expect(releases.get("blob:2")).toHaveBeenCalledOnce();

    wrapper.unmount();
    expect(revoke.mock.calls.filter(([url]) => url === "blob:1")).toHaveLength(1);
    expect(revoke.mock.calls.filter(([url]) => url === "blob:2")).toHaveLength(1);
    expect(releases.get("blob:1")).toHaveBeenCalledOnce();
    expect(releases.get("blob:2")).toHaveBeenCalledOnce();
  });

  it("revokes Blob URLs on replacement, error, and unmount", async () => {
    let serial = 0;
    vi.spyOn(URL, "createObjectURL").mockImplementation(() => `blob:${++serial}`);
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    const { wrapper, session } = mountSession(fakeBridge());

    await session.applySnapshot(readySnapshot(1, 11));
    expect(session.imageUrl.value).toBe("blob:1");

    await session.applySnapshot(readySnapshot(2, 12));
    expect(revoke).toHaveBeenCalledWith("blob:1");
    expect(session.imageUrl.value).toBe("blob:2");

    const { render: _currentRender, ...withoutRender } = readySnapshot(3, 13);
    await session.applySnapshot({
      ...withoutRender,
      status: "error",
      error: { code: "deleted", message: "檔案不存在", parameters: {} },
    });
    expect(revoke).toHaveBeenCalledWith("blob:2");

    await session.applySnapshot(readySnapshot(4, 14));
    wrapper.unmount();
    expect(revoke).toHaveBeenCalledWith("blob:3");
  });

  it("reads a one-time render token only once when a snapshot is duplicated", async () => {
    const bytes = deferred<ArrayBuffer>();
    const readRender = vi.fn(() => bytes.promise);
    const bridge = fakeBridge({ readRender });
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:only");
    const { wrapper, session } = mountSession(bridge);
    const snapshot = readySnapshot(1, 77);

    const first = session.applySnapshot(snapshot);
    const duplicate = session.applySnapshot(snapshot);
    await vi.waitFor(() => expect(readRender).toHaveBeenCalledTimes(1));
    bytes.resolve(new Uint8Array([1]).buffer);
    await Promise.all([first, duplicate]);
    await nextTick();

    expect(session.imageUrl.value).toBe("blob:only");
    wrapper.unmount();
  });

  it("keeps only one binary read active and skips superseded queued renders", async () => {
    const firstBytes = deferred<ArrayBuffer>();
    const readRender = vi.fn((renderId: number) => {
      if (renderId === 1) return firstBytes.promise;
      return Promise.resolve(new Uint8Array([renderId]).buffer);
    });
    const bridge = fakeBridge({ readRender });
    const createUrl = vi
      .spyOn(URL, "createObjectURL")
      .mockReturnValue("blob:latest");
    const { wrapper, session } = mountSession(bridge);

    const first = session.applySnapshot(readySnapshot(1, 1));
    await vi.waitFor(() => expect(readRender).toHaveBeenCalledWith(1));
    const superseded = session.applySnapshot(readySnapshot(2, 2));
    const latest = session.applySnapshot(readySnapshot(3, 3));

    expect(readRender.mock.calls.map(([renderId]) => renderId)).toEqual([1]);
    firstBytes.resolve(new Uint8Array([1]).buffer);
    await Promise.all([first, superseded, latest]);

    expect(readRender.mock.calls.map(([renderId]) => renderId)).toEqual([1, 3]);
    expect(createUrl).toHaveBeenCalledTimes(1);
    expect(session.snapshot.value?.generation).toBe(3);
    expect(session.imageUrl.value).toBe("blob:latest");
    wrapper.unmount();
  });

  it("passes a sliced ArrayBufferView to Blob without copying its backing buffer", async () => {
    const backing = new Uint8Array([9, 1, 2, 8]);
    const view = new Uint8Array(backing.buffer, 1, 2);
    const slice = vi.spyOn(ArrayBuffer.prototype, "slice");
    let blobSize = -1;
    vi.spyOn(URL, "createObjectURL").mockImplementation((blob) => {
      blobSize = blob.size;
      return "blob:view";
    });
    const { wrapper, session } = mountSession(
      fakeBridge({ readRender: vi.fn(async () => view) }),
    );

    await session.applySnapshot(readySnapshot(1, 91));

    expect(slice).not.toHaveBeenCalled();
    expect(blobSize).toBe(2);
    expect(session.imageUrl.value).toBe("blob:view");
    wrapper.unmount();
  });

  it("releases every Blob URL after repeated replacements", async () => {
    let serial = 0;
    const createdUrls: string[] = [];
    const create = vi
      .spyOn(URL, "createObjectURL")
      .mockImplementation(() => {
        const url = `blob:stress-${++serial}`;
        createdUrls.push(url);
        return url;
      });
    const revoke = vi
      .spyOn(URL, "revokeObjectURL")
      .mockImplementation(() => undefined);
    const { wrapper, session } = mountSession(fakeBridge());

    for (let generation = 1; generation <= 50; generation += 1) {
      await session.applySnapshot(readySnapshot(generation, 1000 + generation));
    }
    expect(create).toHaveBeenCalledTimes(50);
    expect(revoke).toHaveBeenCalledTimes(49);

    wrapper.unmount();
    expect(revoke).toHaveBeenCalledTimes(50);
    const revokedUrls = revoke.mock.calls.map(([url]) => url);
    expect(new Set(revokedUrls)).toEqual(new Set(createdUrls));
    for (const url of createdUrls) {
      expect(revokedUrls.filter((revoked) => revoked === url)).toHaveLength(1);
    }
  });

  it("does not create a Blob URL when an in-flight read finishes after unmount", async () => {
    const bytes = deferred<ArrayBuffer>();
    const create = vi.spyOn(URL, "createObjectURL");
    const { wrapper, session } = mountSession(
      fakeBridge({ readRender: vi.fn(() => bytes.promise) }),
    );

    const pending = session.applySnapshot(readySnapshot(1, 123));
    await vi.waitFor(() =>
      expect(session.snapshot.value?.generation).toBe(1),
    );
    wrapper.unmount();
    bytes.resolve(new Uint8Array([1]).buffer);
    await pending;

    expect(create).not.toHaveBeenCalled();
  });
});
