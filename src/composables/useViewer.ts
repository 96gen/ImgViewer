import { computed, nextTick, onMounted, onUnmounted, shallowRef } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  tauriViewerBridge,
  type ViewerBridge,
} from "../services/viewerBridge";
import type {
  NavigationDirection,
  RenderDescriptor,
  ViewerSnapshot,
} from "../types/viewer";

const FALLBACK_ERROR = "無法讀取圖片資料。";

export interface DisplayedImage {
  generation: number;
  fileName: string;
  render: RenderDescriptor;
  url: string;
}

export interface ImagePreload {
  ready: Promise<void>;
  release: () => void;
}

export type ImagePreloader = (url: string) => ImagePreload;

interface PendingImage {
  url: string;
  preload: ImagePreload;
  canceled: Promise<void>;
  cancel: () => void;
  retired: boolean;
}

function messageFrom(error: unknown): string {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message.trim()) return error.message;
  return FALLBACK_ERROR;
}

function asBlobPart(data: ArrayBuffer | ArrayBufferView): BlobPart {
  // Blob accepts an ArrayBufferView and respects its byteOffset/byteLength.
  // Passing the view through avoids allocating an exact-copy ArrayBuffer for
  // binary IPC implementations that return Uint8Array instead of ArrayBuffer.
  return data as BlobPart;
}

const preloadImage: ImagePreloader = (url) => {
  const image = new Image();
  image.decoding = "async";

  let ready: Promise<void>;
  if (typeof image.decode === "function") {
    image.src = url;
    ready = image.decode();
  } else {
    ready = new Promise<void>((resolve, reject) => {
      image.onload = () => resolve();
      image.onerror = () => reject(new Error("WebView2 無法預先解碼圖片。"));
      image.src = url;
    });
  }

  let released = false;
  return {
    ready,
    release: () => {
      if (released) return;
      released = true;
      image.onload = null;
      image.onerror = null;
      image.removeAttribute("src");
    },
  };
};

async function waitForVisiblePaint() {
  await nextTick();
  if (
    typeof window !== "undefined" &&
    typeof window.requestAnimationFrame === "function"
  ) {
    await new Promise<void>((resolve) => {
      let finished = false;
      let timeout = 0;
      let frame = 0;
      const finish = () => {
        if (finished) return;
        finished = true;
        window.clearTimeout(timeout);
        if (frame) window.cancelAnimationFrame(frame);
        resolve();
      };
      timeout = window.setTimeout(finish, 100);
      frame = window.requestAnimationFrame(() => {
        frame = 0;
        finish();
      });
    });
  }
}

export function useViewer(
  bridge: ViewerBridge = tauriViewerBridge,
  prepareImage: ImagePreloader = preloadImage,
) {
  const snapshot = shallowRef<ViewerSnapshot | null>(null);
  const displayedImage = shallowRef<DisplayedImage | null>(null);
  const imageUrl = computed(() => displayedImage.value?.url ?? null);
  const clientError = shallowRef<string | null>(null);

  const unlisteners: UnlistenFn[] = [];
  let requestedRenderId: number | null = null;
  let requestedGeneration = -1;
  let latestRenderRequest = 0;
  let renderReadQueue: Promise<void> = Promise.resolve();
  let newestGeneration = -1;
  let pendingImage: PendingImage | null = null;
  let disposed = false;

  const releaseDisplayedImage = () => {
    const current = displayedImage.value;
    displayedImage.value = null;
    if (current) URL.revokeObjectURL(current.url);
  };

  const retirePendingImage = (candidate: PendingImage) => {
    if (candidate.retired) return;
    candidate.retired = true;
    candidate.cancel();
    candidate.preload.release();
    URL.revokeObjectURL(candidate.url);
    if (pendingImage === candidate) pendingImage = null;
  };

  const invalidateRenderRequests = () => {
    latestRenderRequest += 1;
    requestedRenderId = null;
    requestedGeneration = -1;
    if (pendingImage) retirePendingImage(pendingImage);
  };

  const isCurrentRender = (candidate: ViewerSnapshot, renderId: number) => {
    const current = snapshot.value;
    return (
      !disposed &&
      candidate.generation === newestGeneration &&
      current?.generation === candidate.generation &&
      current.status === "ready" &&
      current.render?.renderId === renderId
    );
  };

  const createRenderUrl = async (
    candidate: ViewerSnapshot,
    renderId: number,
    mimeType: string,
  ) => {
    const bytes = await bridge.readRender(renderId);
    if (!isCurrentRender(candidate, renderId)) return null;

    // Keep the IPC ArrayBuffer in this short stack frame so it can be released
    // before the (potentially slower) WebView image predecode begins.
    const blob = new Blob([asBlobPart(bytes)], { type: mimeType });
    return URL.createObjectURL(blob);
  };

  const readRender = async (candidate: ViewerSnapshot) => {
    const descriptor = candidate.render;
    if (!descriptor) return;

    const renderId = descriptor.renderId;
    if (!isCurrentRender(candidate, renderId)) return;

    let nextUrl: string | null = null;
    try {
      nextUrl = await createRenderUrl(
        candidate,
        renderId,
        descriptor.mimeType,
      );
      if (!nextUrl) return;

      let cancel!: () => void;
      const canceled = new Promise<void>((resolve) => {
        cancel = resolve;
      });
      const next: PendingImage = {
        url: nextUrl,
        preload: prepareImage(nextUrl),
        canceled,
        cancel,
        retired: false,
      };
      pendingImage = next;

      const outcome = await Promise.race([
        next.preload.ready.then(
          () => ({ status: "ready" as const }),
          (error: unknown) => ({ status: "error" as const, error }),
        ),
        next.canceled.then(() => ({ status: "canceled" as const })),
      ]);

      if (outcome.status === "canceled") return;
      if (outcome.status === "error") throw outcome.error;
      if (pendingImage !== next || !isCurrentRender(candidate, renderId)) {
        retirePendingImage(next);
        return;
      }

      // Transfer ownership of the candidate URL to the displayed frame in one
      // reactive assignment. The old frame stays visible through loading and
      // predecode, then survives one paint after the swap before being revoked.
      pendingImage = null;
      const previous = displayedImage.value;
      displayedImage.value = {
        generation: candidate.generation,
        fileName: candidate.fileName ?? "圖片",
        render: descriptor,
        url: next.url,
      };
      clientError.value = null;
      await waitForVisiblePaint();
      next.preload.release();
      if (previous && previous.url !== next.url) {
        URL.revokeObjectURL(previous.url);
      }
    } catch (error) {
      if (pendingImage?.url === nextUrl) {
        retirePendingImage(pendingImage);
      } else if (nextUrl) {
        URL.revokeObjectURL(nextUrl);
      }
      if (isCurrentRender(candidate, renderId)) {
        releaseDisplayedImage();
        clientError.value = messageFrom(error);
      }
    }
  };

  const loadRender = (candidate: ViewerSnapshot): Promise<void> => {
    const descriptor = candidate.render;
    if (
      !descriptor ||
      (displayedImage.value?.generation === candidate.generation &&
        displayedImage.value.render.renderId === descriptor.renderId)
    ) {
      return Promise.resolve();
    }
    if (
      requestedRenderId === descriptor.renderId &&
      requestedGeneration === candidate.generation
    ) {
      return renderReadQueue;
    }

    const request = ++latestRenderRequest;
    requestedRenderId = descriptor.renderId;
    requestedGeneration = candidate.generation;

    // Binary invoke responses cannot be aborted. Serialize them and allow all
    // queued-but-not-started requests except the newest to expire, bounding
    // frontend/IPC payloads to one active buffer plus one opaque Rust token.
    const queued = renderReadQueue
      .then(async () => {
        if (disposed || request !== latestRenderRequest) return;
        await readRender(candidate);
      })
      .finally(() => {
        if (request === latestRenderRequest) {
          requestedRenderId = null;
          requestedGeneration = -1;
        }
      });
    renderReadQueue = queued;
    return queued;
  };

  const applySnapshot = async (candidate: ViewerSnapshot) => {
    if (disposed || candidate.generation < newestGeneration) return;

    const current = snapshot.value;
    if (candidate.generation === newestGeneration && current) {
      const currentIsTerminal =
        current.status === "ready" || current.status === "error";

      // A fast decoder can emit the terminal event before the invoke call
      // resolves with its loading snapshot. Never let that late command
      // response move the same generation backwards. Likewise, an image
      // element error is terminal because its one-time render token has
      // already been consumed.
      if (
        (currentIsTerminal && candidate.status === "loading") ||
        (current.status === "error" && candidate.status === "ready")
      ) {
        return;
      }
    }

    const generationChanged = candidate.generation > newestGeneration;
    if (generationChanged) {
      newestGeneration = candidate.generation;
      invalidateRenderRequests();
      clientError.value = null;
    }

    snapshot.value = candidate;

    // Navigation/loading intentionally keeps the committed frame on screen.
    // Empty/error states are terminal and may release it immediately.
    if (candidate.status === "loading") return;
    if (candidate.status !== "ready" || !candidate.render) {
      invalidateRenderRequests();
      releaseDisplayedImage();
      return;
    }

    await loadRender(candidate);
  };

  const reportCommandError = (error: unknown) => {
    clientError.value = messageFrom(error);
  };

  const openPath = async (path: string) => {
    try {
      await applySnapshot(await bridge.openPath(path));
    } catch (error) {
      reportCommandError(error);
    }
  };

  const chooseAndOpen = async () => {
    try {
      const path = await bridge.chooseImage();
      if (path) await openPath(path);
    } catch (error) {
      reportCommandError(error);
    }
  };

  const navigate = async (direction: NavigationDirection) => {
    const current = snapshot.value;
    if (
      (direction === "previous" && !current?.canPrevious) ||
      (direction === "next" && !current?.canNext)
    ) {
      return;
    }

    try {
      await applySnapshot(await bridge.navigate(direction));
    } catch (error) {
      reportCommandError(error);
    }
  };

  const rememberUnlistener = async (registration: Promise<UnlistenFn>) => {
    try {
      const unlisten = await registration;
      if (disposed) unlisten();
      else unlisteners.push(unlisten);
    } catch (error) {
      if (!disposed) reportCommandError(error);
    }
  };

  const start = async () => {
    // Register the backend event listener before asking for the current
    // snapshot. Otherwise a startup decode can finish in the gap and its
    // ready event can be lost.
    await rememberUnlistener(
      bridge.listenSnapshot((next) => void applySnapshot(next)),
    );
    void rememberUnlistener(
      bridge.listenFileDrop((path) => void openPath(path)),
    );

    try {
      await applySnapshot(await bridge.currentSnapshot());
    } catch (error) {
      reportCommandError(error);
    }
  };

  const dispose = () => {
    if (disposed) return;
    disposed = true;
    invalidateRenderRequests();
    for (const unlisten of unlisteners.splice(0)) unlisten();
    releaseDisplayedImage();
  };

  const displayError = computed(() => {
    if (clientError.value) return clientError.value;
    return snapshot.value?.error?.message ?? null;
  });

  onMounted(() => void start());
  onUnmounted(dispose);

  return {
    snapshot,
    displayedImage,
    imageUrl,
    displayError,
    applySnapshot,
    chooseAndOpen,
    openPath,
    navigate,
    start,
    dispose,
  };
}
