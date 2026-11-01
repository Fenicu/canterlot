import { describe, expect, it } from "vitest";
import { ScrollController } from "#lib/timeline/scrollController.js";

/** Прокручиваемый узел с настраиваемой геометрией: браузер сам прижимает scrollTop к пределу. */
function scroller(content: number, view: number, top = 0) {
  const listeners: (() => void)[] = [];
  let position = top;
  const el = {
    scrollHeight: content,
    clientHeight: view,
    get scrollTop() {
      return position;
    },
    set scrollTop(value: number) {
      position = Math.max(0, Math.min(value, el.scrollHeight - el.clientHeight));
    },
    addEventListener: (_: string, fn: () => void) => listeners.push(fn),
    removeEventListener: () => undefined,
  };
  const fire = () => listeners.forEach((fn) => fn());
  return {
    el: el as unknown as HTMLElement,
    scroll(to: number) {
      el.scrollTop = to;
      fire();
    },
    /** Окно стало выше: предел прокрутки снизился, браузер прижал scrollTop и прислал scroll. */
    grow(view: number, content = el.scrollHeight) {
      el.clientHeight = view;
      const clampedTo = el.scrollHeight - el.clientHeight;
      el.scrollHeight = content;
      const max = clampedTo;
      if (el.scrollTop > max) {
        el.scrollTop = max;
        fire();
      }
    },
    shrink(view: number) {
      el.clientHeight = view;
    },
  };
}

const env = { raf: (fn: () => void) => (fn(), 1), cancelRaf: () => undefined };

describe("timeline/ScrollController", () => {
  it("a_scroll_clamped_by_a_taller_window_is_not_the_owner_leaving_the_bottom", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el, env);
    controller.onContentChange("resize");
    view.shrink(400);
    controller.onContentChange("resize");
    view.scroll(600);
    expect(controller.pinned).toBe(true);
    view.grow(500);
    expect(controller.pinned).toBe(true);
    controller.onContentChange("tail-growth");
    expect(controller.showJump).toBe(false);
  });

  it("a_clamp_in_the_frame_where_a_new_tail_arrives_is_not_the_owner_either", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el, env);
    view.shrink(400);
    controller.onContentChange("resize");
    view.scroll(600);
    view.grow(500, 1135);
    expect(controller.pinned).toBe(true);
    controller.onContentChange("tail-growth");
    expect(controller.showJump).toBe(false);
  });

  it("the_owner_scrolling_up_still_unpins", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el, env);
    view.scroll(200);
    expect(controller.pinned).toBe(false);
  });
});
