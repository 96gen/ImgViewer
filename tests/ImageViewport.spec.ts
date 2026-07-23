import { mount, type VueWrapper } from "@vue/test-utils";
import { nextTick } from "vue";
import { describe, expect, it, vi } from "vitest";
import ImageViewport from "../src/components/ImageViewport.vue";

function viewportRect(width: number, height: number): DOMRect {
  return {
    x: 0,
    y: 0,
    width,
    height,
    top: 0,
    right: width,
    bottom: height,
    left: 0,
    toJSON: () => ({}),
  } as DOMRect;
}

async function pointerClick(
  wrapper: VueWrapper,
  selector: string,
  pointerId: number,
) {
  const button = wrapper.get(selector);
  dispatchPointer(button.element, "pointerdown", pointerId);
  await nextTick();
  dispatchPointer(button.element, "pointerup", pointerId);
  await nextTick();
  await button.trigger("click");
}

function dispatchPointer(
  element: Element,
  type: string,
  pointerId: number,
  clientX = 350,
  clientY = 680,
) {
  const event = new MouseEvent(type, {
    bubbles: true,
    cancelable: true,
    button: 0,
    clientX,
    clientY,
  });
  Object.defineProperty(event, "pointerId", { value: pointerId });
  element.dispatchEvent(event);
}

describe("ImageViewport zoom controls", () => {
  it("keeps all controls clickable while the image can be panned", async () => {
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
      viewportRect(696, 700),
    );
    const wrapper = mount(ImageViewport, {
      attachTo: document.body,
      props: {
        src: "blob:test",
        fileName: "large.png",
        width: 1000,
        height: 600,
      },
    });
    const viewport = wrapper.get('[data-testid="image-viewport"]');
    const setPointerCapture = vi.fn();
    const releasePointerCapture = vi.fn();
    Object.defineProperties(viewport.element, {
      setPointerCapture: { value: setPointerCapture },
      releasePointerCapture: { value: releasePointerCapture },
    });
    await nextTick();

    expect(wrapper.get("output").text()).toBe("70%");

    // 69.6% * 1.25 = 87%. The image is now wider than the viewport,
    // which used to make the parent steal the 100% button's click.
    await wrapper.get('button[aria-label="放大"]').trigger("click");
    expect(wrapper.get("output").text()).toBe("87%");
    await pointerClick(wrapper, 'button[title^="實際大小"]', 1);
    expect(wrapper.get("output").text()).toBe("100%");
    expect(wrapper.get("img").attributes("style")).toContain("scale(1)");

    await pointerClick(wrapper, 'button[aria-label="放大"]', 2);
    expect(wrapper.get("output").text()).toBe("125%");
    await pointerClick(wrapper, 'button[aria-label="放大"]', 3);
    expect(wrapper.get("output").text()).toBe("156%");
    expect(wrapper.get("img").attributes("style")).toContain("scale(1.5625)");

    await pointerClick(wrapper, 'button[aria-label="縮小"]', 4);
    expect(wrapper.get("output").text()).toBe("125%");

    await pointerClick(wrapper, 'button[title^="符合視窗"]', 5);
    expect(wrapper.get("output").text()).toBe("70%");
    expect(wrapper.get("img").attributes("style")).toContain("scale(0.696)");

    expect(setPointerCapture).not.toHaveBeenCalled();
    expect(releasePointerCapture).not.toHaveBeenCalled();
    expect(viewport.classes()).not.toContain("is-dragging");
    wrapper.unmount();
  });

  it("still starts panning when pointerdown occurs on the viewport", async () => {
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
      viewportRect(400, 400),
    );
    const wrapper = mount(ImageViewport, {
      attachTo: document.body,
      props: {
        src: "blob:test",
        fileName: "large.png",
        width: 1000,
        height: 600,
      },
    });
    const viewport = wrapper.get('[data-testid="image-viewport"]');
    const setPointerCapture = vi.fn();
    Object.defineProperty(viewport.element, "setPointerCapture", {
      value: setPointerCapture,
    });
    await nextTick();

    await wrapper.get('button[title^="實際大小"]').trigger("click");
    dispatchPointer(viewport.element, "pointerdown", 8);
    await nextTick();

    expect(setPointerCapture).toHaveBeenCalledTimes(1);
    expect(viewport.classes()).toContain("is-dragging");
    wrapper.unmount();
  });

  it("coalesces repeated pointer moves into one animation-frame update", async () => {
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
      viewportRect(400, 400),
    );
    let frame: FrameRequestCallback | null = null;
    const requestFrame = vi
      .spyOn(window, "requestAnimationFrame")
      .mockImplementation((callback) => {
        frame = callback;
        return 17;
      });
    const cancelFrame = vi
      .spyOn(window, "cancelAnimationFrame")
      .mockImplementation(() => undefined);
    const wrapper = mount(ImageViewport, {
      attachTo: document.body,
      props: {
        src: "blob:test",
        fileName: "large.png",
        width: 1000,
        height: 600,
      },
    });
    const viewport = wrapper.get('[data-testid="image-viewport"]');
    Object.defineProperties(viewport.element, {
      setPointerCapture: { value: vi.fn() },
      releasePointerCapture: { value: vi.fn() },
    });
    await nextTick();
    await wrapper.get('button[title^="實際大小"]').trigger("click");

    dispatchPointer(viewport.element, "pointerdown", 9, 200, 200);
    dispatchPointer(viewport.element, "pointermove", 9, 210, 205);
    dispatchPointer(viewport.element, "pointermove", 9, 220, 212);
    dispatchPointer(viewport.element, "pointermove", 9, 230, 220);

    expect(requestFrame).toHaveBeenCalledTimes(1);
    expect(wrapper.get("img").attributes("style")).toContain(
      "translate(0px, 0px)",
    );
    const scheduledFrame = frame as FrameRequestCallback | null;
    if (!scheduledFrame) throw new Error("expected a scheduled drag frame");
    scheduledFrame(0);
    await nextTick();
    expect(wrapper.get("img").attributes("style")).toContain(
      "translate(30px, 20px)",
    );

    dispatchPointer(viewport.element, "pointerup", 9, 230, 220);
    expect(cancelFrame).not.toHaveBeenCalled();
    wrapper.unmount();
  });
});

describe("ImageViewport image switching", () => {
  it("keeps the same DOM viewport and returns to Fit when src and dimensions change", async () => {
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
      viewportRect(400, 400),
    );
    const wrapper = mount(ImageViewport, {
      attachTo: document.body,
      props: {
        src: "blob:landscape",
        fileName: "landscape.png",
        width: 1000,
        height: 600,
      },
    });
    const viewport = wrapper.get('[data-testid="image-viewport"]').element;
    const image = wrapper.get("img").element;
    await nextTick();

    await wrapper.get('button[title^="實際大小"]').trigger("click");
    await wrapper.get('button[aria-label="放大"]').trigger("click");
    expect(wrapper.get("output").text()).toBe("125%");

    await wrapper.setProps({
      src: "blob:portrait",
      fileName: "portrait.png",
      width: 200,
      height: 800,
    });

    expect(wrapper.get('[data-testid="image-viewport"]').element).toBe(viewport);
    expect(wrapper.get("img").element).toBe(image);
    expect(wrapper.get("output").text()).toBe("50%");
    expect(wrapper.get("img").attributes("style")).toContain("width: 200px");
    expect(wrapper.get("img").attributes("style")).toContain("height: 800px");
    expect(wrapper.get("img").attributes("style")).toContain("scale(0.5)");
    wrapper.unmount();
  });

  it("reports the actual failing image src", async () => {
    const wrapper = mount(ImageViewport, {
      props: {
        src: "blob:declared",
        fileName: "broken.png",
        width: 100,
        height: 80,
      },
    });
    const image = wrapper.get("img");
    Object.defineProperty(image.element, "currentSrc", {
      configurable: true,
      value: "blob:actually-failed",
    });

    await image.trigger("error");

    expect(wrapper.emitted("imageError")).toEqual([["blob:actually-failed"]]);
    wrapper.unmount();
  });
});
