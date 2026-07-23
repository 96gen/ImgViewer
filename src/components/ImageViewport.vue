<script setup lang="ts">
import {
  computed,
  onBeforeUnmount,
  onMounted,
  reactive,
  ref,
  watch,
} from "vue";
import {
  clampPan,
  clampZoom,
  fitScale,
  wheelZoom,
  zoomAroundPoint,
  type Point,
  type Size,
} from "../viewer/geometry";

const props = defineProps<{
  src: string;
  fileName: string;
  width: number;
  height: number;
}>();

const emit = defineEmits<{
  imageError: [src: string];
}>();

const viewportElement = ref<HTMLElement | null>(null);
const viewport = reactive<Size>({ width: 0, height: 0 });
const offset = reactive<Point>({ x: 0, y: 0 });
const scale = ref(1);
const mode = ref<"fit" | "manual">("fit");
const dragging = ref(false);
let dragPointerId: number | null = null;
let previousPointer: Point = { x: 0, y: 0 };
let pendingPointer: Point | null = null;
let pointerFrame: number | null = null;
let resizeObserver: ResizeObserver | null = null;

const imageSize = computed<Size>(() => ({
  width: props.width,
  height: props.height,
}));

const currentFitScale = computed(() => fitScale(imageSize.value, viewport));
const zoomPercent = computed(() => Math.round(scale.value * 100));
const canPan = computed(
  () =>
    props.width * scale.value > viewport.width + 0.5 ||
    props.height * scale.value > viewport.height + 0.5,
);

const imageStyle = computed(() => ({
  width: `${props.width}px`,
  height: `${props.height}px`,
  transform: `translate(-50%, -50%) translate(${offset.x}px, ${offset.y}px) scale(${scale.value})`,
}));

const updateViewportSize = () => {
  const element = viewportElement.value;
  if (!element) return;
  const rect = element.getBoundingClientRect();
  if (viewport.width === rect.width && viewport.height === rect.height) return;
  viewport.width = rect.width;
  viewport.height = rect.height;

  if (mode.value === "fit") {
    scale.value = currentFitScale.value;
    offset.x = 0;
    offset.y = 0;
  } else {
    const clamped = clampPan(offset, imageSize.value, viewport, scale.value);
    offset.x = clamped.x;
    offset.y = clamped.y;
  }
};

const fit = () => {
  mode.value = "fit";
  scale.value = currentFitScale.value;
  offset.x = 0;
  offset.y = 0;
};

const actualSize = () => {
  mode.value = "manual";
  scale.value = 1;
  offset.x = 0;
  offset.y = 0;
};

const setManualScale = (nextScale: number, anchor: Point = { x: 0, y: 0 }) => {
  const next = clampZoom(nextScale);
  const nextOffset = zoomAroundPoint(offset, anchor, scale.value, next);
  const clamped = clampPan(nextOffset, imageSize.value, viewport, next);
  mode.value = "manual";
  scale.value = next;
  offset.x = clamped.x;
  offset.y = clamped.y;
};

const zoomIn = () => setManualScale(scale.value * 1.25);
const zoomOut = () => setManualScale(scale.value / 1.25);

const onWheel = (event: WheelEvent) => {
  const element = viewportElement.value;
  if (!element) return;
  const rect = element.getBoundingClientRect();
  const anchor = {
    x: event.clientX - rect.left - rect.width / 2,
    y: event.clientY - rect.top - rect.height / 2,
  };
  setManualScale(wheelZoom(scale.value, event.deltaY), anchor);
};

const onPointerDown = (event: PointerEvent) => {
  // Overlay buttons must never start panning or take pointer capture. This
  // keeps +, -, Fit, and 100% clickable whenever the image can be panned.
  if (
    event.target instanceof Element &&
    event.target.closest(".zoom-controls")
  ) {
    return;
  }
  if (event.button !== 0 || !canPan.value) return;
  dragging.value = true;
  dragPointerId = event.pointerId;
  pendingPointer = null;
  previousPointer = { x: event.clientX, y: event.clientY };
  viewportElement.value?.setPointerCapture?.(event.pointerId);
};

const commitPendingPointer = () => {
  pointerFrame = null;
  const nextPointer = pendingPointer;
  pendingPointer = null;
  if (!nextPointer || !dragging.value) return;
  const proposed = {
    x: offset.x + nextPointer.x - previousPointer.x,
    y: offset.y + nextPointer.y - previousPointer.y,
  };
  previousPointer = nextPointer;
  const clamped = clampPan(proposed, imageSize.value, viewport, scale.value);
  offset.x = clamped.x;
  offset.y = clamped.y;
};

const onPointerMove = (event: PointerEvent) => {
  if (!dragging.value || dragPointerId !== event.pointerId) return;
  pendingPointer = { x: event.clientX, y: event.clientY };
  if (pointerFrame === null) {
    pointerFrame = window.requestAnimationFrame(commitPendingPointer);
  }
};

const stopDragging = (event?: PointerEvent) => {
  if (event && dragPointerId !== event.pointerId) return;
  if (pointerFrame !== null) {
    window.cancelAnimationFrame(pointerFrame);
    pointerFrame = null;
  }
  commitPendingPointer();
  if (dragPointerId !== null) {
    viewportElement.value?.releasePointerCapture?.(dragPointerId);
  }
  dragging.value = false;
  dragPointerId = null;
};

watch(
  () => [props.src, props.width, props.height] as const,
  () => {
    stopDragging();
    fit();
  },
);

const onImageError = (event: Event) => {
  const image = event.currentTarget as HTMLImageElement;
  emit("imageError", image.currentSrc || image.src);
};

onMounted(() => {
  updateViewportSize();
  if (typeof ResizeObserver !== "undefined") {
    resizeObserver = new ResizeObserver(updateViewportSize);
    if (viewportElement.value) resizeObserver.observe(viewportElement.value);
  } else {
    window.addEventListener("resize", updateViewportSize);
  }
  fit();
});

onBeforeUnmount(() => {
  if (pointerFrame !== null) window.cancelAnimationFrame(pointerFrame);
  pendingPointer = null;
  resizeObserver?.disconnect();
  window.removeEventListener("resize", updateViewportSize);
});

defineExpose({ fit, actualSize, zoomIn, zoomOut });
</script>

<template>
  <div
    ref="viewportElement"
    class="image-viewport"
    :class="{ 'is-pannable': canPan, 'is-dragging': dragging }"
    data-testid="image-viewport"
    @wheel.prevent="onWheel"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="stopDragging"
    @pointercancel="stopDragging"
  >
    <img
      class="viewer-image"
      :src="src"
      :alt="fileName"
      :style="imageStyle"
      draggable="false"
      @error="onImageError"
    />

    <div
      class="zoom-controls"
      aria-label="縮放控制"
      @pointerdown.stop
      @pointermove.stop
      @pointerup.stop
      @pointercancel.stop
    >
      <button type="button" title="縮小" aria-label="縮小" @click="zoomOut">−</button>
      <output aria-live="polite">{{ zoomPercent }}%</output>
      <button type="button" title="放大" aria-label="放大" @click="zoomIn">＋</button>
      <button type="button" title="符合視窗（Ctrl+0）" @click="fit">符合視窗</button>
      <button type="button" title="實際大小（Ctrl+1）" @click="actualSize">100%</button>
    </div>
  </div>
</template>
