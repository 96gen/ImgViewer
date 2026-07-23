import { afterEach, vi } from "vitest";

class ResizeObserverMock implements ResizeObserver {
  readonly observed = new Set<Element>();

  constructor(private readonly callback: ResizeObserverCallback) {}

  observe(target: Element) {
    this.observed.add(target);
    this.callback(
      [
        {
          target,
          contentRect: target.getBoundingClientRect(),
        } as ResizeObserverEntry,
      ],
      this,
    );
  }

  unobserve(target: Element) {
    this.observed.delete(target);
  }

  disconnect() {
    this.observed.clear();
  }
}

vi.stubGlobal("ResizeObserver", ResizeObserverMock);

afterEach(() => {
  document.body.innerHTML = "";
  vi.restoreAllMocks();
});
