import { describe, expect, it } from "vitest";
import { ScrollController } from "#lib/timeline/scrollController.js";

/** Прокручиваемый узел с настраиваемой геометрией: браузер сам прижимает scrollTop к пределу. */
function scroller(content: number, view: number, top = 0) {
  const listeners = new Map<string, (() => void)[]>();
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
    addEventListener: (type: string, fn: () => void) => listeners.set(type, [...(listeners.get(type) ?? []), fn]),
    removeEventListener: () => undefined,
  };
  const emit = (type: string) => (listeners.get(type) ?? []).forEach((fn) => fn());
  const fire = () => emit("scroll");
  return {
    el: el as unknown as HTMLElement,
    scroll(to: number) {
      el.scrollTop = to;
      fire();
    },
    /** Колесо владельца: событие wheel приходит раньше сдвига. */
    wheel(to: number) {
      emit("wheel");
      el.scrollTop = to;
      fire();
    },
    emit,
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
    /** Промежуточная раскладка без события: высота проседает (браузер прижимает scrollTop), потом снова растёт. */
    dip(content: number, after: number) {
      el.scrollHeight = content;
      el.scrollTop = position;
      return () => {
        el.scrollHeight = after;
      };
    },
    fire,
  };
}

describe("timeline/ScrollController", () => {
  it("a_scroll_clamped_by_a_taller_window_is_not_the_owner_leaving_the_bottom", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el);
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
    const controller = new ScrollController(view.el);
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
    const controller = new ScrollController(view.el);
    view.wheel(200);
    expect(controller.pinned).toBe(false);
  });

  it("a_scroll_without_a_gesture_on_an_unchanged_feed_is_still_the_owner", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el);
    view.scroll(200);
    expect(controller.pinned).toBe(false);
  });

  it("a_climb_without_a_gesture_on_a_changed_feed_returns_to_the_bottom", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el);
    controller.jumpToBottom();
    // Хвост на миг стал короче, браузер прижал scrollTop, к событию прокрутки хвост вырос снова.
    const el = view.el as unknown as { scrollHeight: number };
    el.scrollHeight = 900;
    view.el.scrollTop = 400;
    el.scrollHeight = 1100;
    view.fire();
    expect(controller.pinned).toBe(true);
    expect(view.el.scrollTop).toBe(600);
  });

  it("a_climb_after_a_content_change_without_a_new_height_returns_to_the_bottom", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el);
    controller.jumpToBottom();
    // Строка домерилась той же высоты, но scrollTop успел прижаться к промежуточной раскладке.
    controller.onContentChange("height-above");
    view.el.scrollTop = 300;
    view.fire();
    expect(controller.pinned).toBe(true);
    expect(view.el.scrollTop).toBe(500);
  });

  it("touch_momentum_after_the_finger_lifts_still_belongs_to_the_owner", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el);
    view.emit("touchstart");
    view.emit("touchend");
    (view.el as unknown as { scrollHeight: number }).scrollHeight = 1200;
    view.scroll(300);
    expect(controller.pinned).toBe(false);
  });

  it("tail_growth_reaches_the_bottom_before_the_next_frame", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el);
    (view.el as unknown as { scrollHeight: number }).scrollHeight = 1200;
    controller.onContentChange("tail-growth");
    // Следующий кадр ещё не наступил: подвал не должен успеть уехать вверх.
    expect(view.el.scrollTop).toBe(700);
  });

  it("a_move_the_owner_made_before_the_scroll_event_keeps_the_tail_growth_from_yanking_the_feed", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el);
    // Владелец уже сдвинул ленту, но событие scroll ещё не дошло.
    view.emit("wheel");
    view.el.scrollTop = 200;
    controller.onContentChange("tail-growth");
    expect(view.el.scrollTop).toBe(200);
    expect(controller.pinned).toBe(false);
    expect(controller.showJump).toBe(true);
  });

  it("a_move_before_the_resize_does_not_look_like_the_owner_is_still_at_the_bottom", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el);
    view.emit("wheel");
    view.el.scrollTop = 200;
    controller.onContentChange("resize");
    expect(view.el.scrollTop).toBe(200);
    expect(controller.pinned).toBe(false);
  });

  it("jump_to_bottom_after_an_unreported_move_still_reaches_the_bottom", () => {
    const view = scroller(1000, 500, 500);
    const controller = new ScrollController(view.el);
    view.el.scrollTop = 200;
    controller.jumpToBottom();
    expect(view.el.scrollTop).toBe(500);
    expect(controller.pinned).toBe(true);
  });

  it("a_clamp_on_a_passing_layout_that_regrew_before_the_scroll_event_keeps_the_bottom", () => {
    const view = scroller(18_000, 600, 17_400);
    const controller = new ScrollController(view.el);
    controller.jumpToBottom();
    // Окно перестраивается: на одной из раскладок лента на миг ниже, scrollTop прижат к ней.
    const regrow = view.dip(15_000, 18_000);
    regrow();
    // Перестройку окна видно по правкам ленты, даже если высота вернулась к прежней.
    controller.onContentChange("height-above");
    // Событие прокрутки от прижатия приходит, когда высота уже вернулась.
    view.fire();
    expect(controller.pinned).toBe(true);
    expect(view.el.scrollTop).toBe(17_400);
  });

  it("a_wheel_up_while_the_feed_changes_still_unpins", () => {
    const view = scroller(18_000, 600, 17_400);
    const controller = new ScrollController(view.el);
    (view.el as unknown as { scrollHeight: number }).scrollHeight = 18_200;
    view.wheel(9_000);
    expect(controller.pinned).toBe(false);
  });
});
