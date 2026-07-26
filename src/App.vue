<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import packageInfo from "../package.json";
import ImageViewport from "./components/ImageViewport.vue";
import { useViewer } from "./composables/useViewer";

const RELEASES_URL = "https://github.com/96gen/ImgViewer/releases";
const viewer = useViewer();
const viewport = ref<InstanceType<typeof ImageViewport> | null>(null);
const releaseUrlInput = ref<HTMLInputElement | null>(null);
const showAbout = ref(false);
const copyStatus = ref("");

const snapshot = computed(() => viewer.snapshot.value);
const displayed = computed(() => viewer.displayedImage.value);
const render = computed(() => displayed.value?.render ?? null);
const hasImage = computed(() => Boolean(displayed.value));
const isSwitching = computed(() => {
  const current = snapshot.value;
  const visible = displayed.value;
  return Boolean(
    current &&
      visible &&
      (current.status === "loading" || viewer.renderPending.value),
  );
});
const isInitialLoading = computed(() => {
  const current = snapshot.value;
  return Boolean(
    !displayed.value &&
      !viewer.displayError.value &&
      (current?.status === "loading" || current?.status === "ready"),
  );
});
const positionLabel = computed(() => {
  const current = snapshot.value;
  if (!current || current.index === null || current.total === 0) return "";
  return `${current.index + 1} / ${current.total}`;
});

const onImageError = (failedSrc: string) => {
  const current = snapshot.value;
  const visible = displayed.value;
  if (
    !current ||
    !visible ||
    visible.url !== failedSrc ||
    current.generation !== visible.generation ||
    current.status !== "ready" ||
    current.render?.renderId !== visible.render.renderId
  ) {
    return;
  }
  const { render: _discardedRender, ...withoutRender } = current;
  void viewer.applySnapshot({
    ...withoutRender,
    status: "error",
    error: {
      code: "webview-image-error",
      message: "WebView2 無法顯示這張圖片。",
      parameters: {},
    },
  });
};

const onKeyDown = (event: KeyboardEvent) => {
  if (event.key === "Escape" && showAbout.value) {
    event.preventDefault();
    showAbout.value = false;
    return;
  }

  if ((event.ctrlKey || event.metaKey) && !event.altKey) {
    if (event.key.toLowerCase() === "o") {
      event.preventDefault();
      void viewer.chooseAndOpen();
      return;
    }
    if (event.key === "0") {
      event.preventDefault();
      viewport.value?.fit();
      return;
    }
    if (event.key === "1") {
      event.preventDefault();
      viewport.value?.actualSize();
      return;
    }
  }

  if (event.ctrlKey || event.metaKey || event.altKey) return;
  if (event.key === "ArrowLeft") {
    event.preventDefault();
    void viewer.navigate("previous");
  } else if (event.key === "ArrowRight") {
    event.preventDefault();
    void viewer.navigate("next");
  }
};

const copyReleaseUrl = async () => {
  copyStatus.value = "";
  try {
    await navigator.clipboard.writeText(RELEASES_URL);
    copyStatus.value = "已複製";
  } catch {
    releaseUrlInput.value?.select();
    copyStatus.value = "請按 Ctrl+C 複製";
  }
};

const dismissClientError = () => {
  viewer.clearClientError();
};

onMounted(() => window.addEventListener("keydown", onKeyDown));
onBeforeUnmount(() => window.removeEventListener("keydown", onKeyDown));
</script>

<template>
  <main class="app-shell">
    <header class="toolbar">
      <button class="open-button" type="button" title="開啟圖片（Ctrl+O）" @click="viewer.chooseAndOpen">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M3.5 6.5h6l2 2h9v10h-17z" />
          <path d="M3.5 8.5v-3h6l2 2h5" />
        </svg>
        開啟圖片
      </button>

      <div class="file-summary" aria-live="polite">
        <strong>{{ snapshot?.fileName || "尚未開啟圖片" }}</strong>
        <span v-if="positionLabel">{{ positionLabel }}</span>
      </div>

      <div class="toolbar-actions">
        <span v-if="render?.animated" class="animation-badge">動畫</span>
        <button
          class="about-button"
          type="button"
          title="關於 ImgViewer"
          aria-label="關於 ImgViewer"
          @click="showAbout = true"
        >
          ⓘ
        </button>
      </div>
    </header>

    <section class="viewer-area" aria-label="圖片檢視區">
      <ImageViewport
        v-if="hasImage && displayed && render"
        ref="viewport"
        :src="displayed.url"
        :file-name="displayed.fileName"
        :width="render.width"
        :height="render.height"
        @image-error="onImageError"
      />

      <div v-else-if="isInitialLoading" class="state-panel loading-state" role="status">
        <span class="spinner" aria-hidden="true" />
        <p>正在載入 {{ snapshot.fileName || "圖片" }}…</p>
      </div>

      <div v-else-if="viewer.displayError.value" class="state-panel error-state" role="alert">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M12 3 2.5 20h19z" />
          <path d="M12 8v6m0 3v.1" />
        </svg>
        <h2>無法顯示圖片</h2>
        <p>{{ viewer.displayError.value }}</p>
        <small v-if="snapshot?.error?.code">錯誤代碼：{{ snapshot.error.code }}</small>
      </div>

      <div v-else class="state-panel empty-state">
        <svg viewBox="0 0 64 64" aria-hidden="true">
          <rect x="9" y="12" width="46" height="40" rx="5" />
          <circle cx="23" cy="25" r="5" />
          <path d="m13 47 12-13 8 8 7-7 11 12" />
        </svg>
        <h1>開啟一張圖片</h1>
        <p>按 Ctrl+O，或將圖片拖曳到這裡</p>
        <button type="button" @click="viewer.chooseAndOpen">選擇圖片</button>
      </div>

      <div v-if="isSwitching" class="switching-indicator" role="status">
        <span class="mini-spinner" aria-hidden="true" />
        載入中
      </div>

      <div
        v-if="hasImage && viewer.clientError.value"
        class="client-error-banner"
        role="alert"
      >
        <span>{{ viewer.clientError.value }}</span>
        <button type="button" aria-label="關閉錯誤提示" @click="dismissClientError">
          ×
        </button>
      </div>

      <button
        class="nav-button previous"
        type="button"
        aria-label="上一張"
        title="上一張（←）"
        :disabled="!snapshot?.canPrevious"
        @click="viewer.navigate('previous')"
      >
        ‹
      </button>
      <button
        class="nav-button next"
        type="button"
        aria-label="下一張"
        title="下一張（→）"
        :disabled="!snapshot?.canNext"
        @click="viewer.navigate('next')"
      >
        ›
      </button>
    </section>

    <div
      v-if="showAbout"
      class="about-backdrop"
      role="presentation"
      @pointerdown.self="showAbout = false"
    >
      <section
        class="about-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="about-title"
      >
        <button
          class="about-close"
          type="button"
          aria-label="關閉"
          @click="showAbout = false"
        >
          ×
        </button>
        <h2 id="about-title">ImgViewer {{ packageInfo.version }}</h2>
        <p>Windows 專用、離線、唯讀的圖片瀏覽器。</p>
        <p class="about-privacy">不含遙測、自動上傳或程式內更新檢查。</p>
        <label for="release-url">版本發布頁</label>
        <div class="release-url-row">
          <input
            id="release-url"
            ref="releaseUrlInput"
            :value="RELEASES_URL"
            readonly
            spellcheck="false"
            @focus="releaseUrlInput?.select()"
          />
          <button type="button" @click="copyReleaseUrl">複製</button>
        </div>
        <small aria-live="polite">{{ copyStatus }}</small>
      </section>
    </div>
  </main>
</template>
