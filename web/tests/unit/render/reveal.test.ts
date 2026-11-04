import { describe, expect, it } from "vitest";
import { TextReveal, caretInTail, markRevealWords } from "#lib/render/reveal.js";

function words(count: number): string {
  return Array.from({ length: count }, (_, index) => `слово${index}`).join(" ");
}

describe("TextReveal", () => {
  it("reveals_at_a_steady_word_pace", () => {
    const reveal = new TextReveal();
    const full = words(10);
    reveal.update(full, { final: false, now: 0, instant: false });
    // Первое слово видно сразу, остальное идёт своим темпом.
    expect(reveal.text).toBe("слово0");

    reveal.tick(100);
    expect(reveal.shownWords).toBeGreaterThan(1);
    expect(reveal.shownWords).toBeLessThan(7);

    reveal.tick(200);
    expect(reveal.shownWords).toBeGreaterThan(3);
    expect(full.startsWith(reveal.text)).toBe(true);
  });

  it("accelerates_when_the_backlog_grows_and_keeps_lag_bounded", () => {
    const reveal = new TextReveal(32, 0.6);
    let full = words(20);
    reveal.update(full, { final: false, now: 0, instant: false });
    let now = 0;
    for (let step = 1; step <= 120; step++) {
      now += 16;
      // Приходит быстрее базового темпа: отставание должно оставаться в пределе.
      if (step % 6 === 0) {
        for (let index = 0; index < 5; index++)
          full += ` следующее${step}_${index}`;
        reveal.update(full, { final: false, now, instant: false });
      }
      reveal.tick(now);
      expect(reveal.lagSeconds).toBeLessThanOrEqual(0.6 + 1e-9);
    }
    for (let step = 0; step < 300; step++) {
      now += 16;
      reveal.tick(now);
      expect(reveal.lagSeconds).toBeLessThanOrEqual(0.6 + 1e-9);
    }
    expect(reveal.done).toBe(true);
    expect(reveal.text).toBe(full);
  });

  it("finishes_quickly_when_the_block_becomes_final", () => {
    const reveal = new TextReveal();
    const full = words(200);
    reveal.update(full, { final: false, now: 0, instant: false });
    reveal.tick(16);
    expect(reveal.done).toBe(false);

    reveal.update(full, { final: true, now: 20, instant: false, finishMs: 180 });
    reveal.tick(100);
    expect(reveal.done).toBe(false);
    reveal.tick(200);
    expect(reveal.done).toBe(true);
    expect(reveal.text).toBe(full);
  });

  it("shows_everything_at_once_when_motion_is_off", () => {
    const reveal = new TextReveal();
    const full = words(50);
    reveal.update(full, { final: false, now: 0, instant: true });
    expect(reveal.text).toBe(full);
    expect(reveal.done).toBe(true);
  });

  it("applies_the_speed_multiplier", () => {
    const slow = new TextReveal();
    const fast = new TextReveal();
    const full = words(20);
    slow.update(full, { final: false, now: 0, instant: false, scale: 1.5 });
    fast.update(full, { final: false, now: 0, instant: false, scale: 0.5 });
    slow.tick(500);
    fast.tick(500);
    expect(fast.shownWords).toBeGreaterThan(slow.shownWords);
    expect(slow.shownWords).toBeLessThan(20);
  });

  it("keeps_the_final_text_exactly_the_received_one", () => {
    const reveal = new TextReveal();
    const full = "Привет,\n\nмир — и **разметка** [ссылка](https://e.x).";
    reveal.update(full, { final: false, now: 0, instant: false });
    reveal.tick(10_000);
    expect(reveal.done).toBe(true);
    expect(reveal.text).toBe(full);
  });

  it("does_not_replay_old_text_when_a_stream_mounts_mid_flight", () => {
    const reveal = new TextReveal();
    reveal.update("уже пришло", { final: false, now: 0, instant: true });
    expect(reveal.text).toBe("уже пришло");

    reveal.update("уже пришло и ещё", { final: false, now: 16, instant: false });
    expect(reveal.text).toBe("уже пришло");
    reveal.tick(64);
    expect("уже пришло и ещё".startsWith(reveal.text)).toBe(true);
  });
});

describe("markRevealWords", () => {
  it("wraps_only_the_last_words_and_keeps_the_text", () => {
    const html = "<p>первое второе <strong>третье</strong></p>";
    const marked = markRevealWords(html, 2);
    const host = new DOMParser().parseFromString(marked, "text/html").body;
    expect(host.textContent).toBe("первое второе третье");
    const spans = host.querySelectorAll(".reveal-word");
    expect(spans).toHaveLength(2);
    expect(spans[0]?.textContent).toBe("второе");
    expect(spans[1]?.textContent).toBe("третье");
    expect(host.querySelector("strong .reveal-word")?.textContent).toBe(
      "третье",
    );
  });

  it("returns_the_html_untouched_when_nothing_is_new", () => {
    const html = "<p>готово</p>";
    expect(markRevealWords(html, 0)).toBe(html);
  });
});

describe("caretInTail", () => {
  it("puts_the_caret_inside_the_last_list_item", () => {
    const html = "<ul>\n<li>первый</li>\n<li>второй</li>\n</ul>\n";
    const host = new DOMParser().parseFromString(caretInTail(html), "text/html").body;
    expect(host.querySelectorAll(".caret")).toHaveLength(1);
    expect(host.querySelector("ul > li:last-child > .caret")).not.toBeNull();
    expect(host.querySelector("ul")?.nextSibling?.textContent?.trim()).toBe("");
  });

  it("puts_the_caret_inside_the_last_paragraph_and_pre", () => {
    const paragraph = new DOMParser().parseFromString(caretInTail("<p>ответ</p>"), "text/html").body;
    expect(paragraph.querySelector("p > .caret")).not.toBeNull();
    const code = new DOMParser().parseFromString(caretInTail("<pre><code>код</code></pre>"), "text/html").body;
    expect(code.querySelector("pre .caret")).not.toBeNull();
  });

  it("puts_the_caret_before_the_final_newline_so_it_takes_no_extra_line", () => {
    const code = new DOMParser().parseFromString(
      caretInTail("<pre><code>строка 1\nстрока 2\n</code></pre>"),
      "text/html",
    ).body;
    const caret = code.querySelector("pre > code > .caret");
    expect(caret).not.toBeNull();
    expect(caret?.nextSibling?.textContent).toBe("\n");
  });

  it("puts_the_caret_before_a_trailing_break", () => {
    const paragraph = new DOMParser().parseFromString(caretInTail("<p>ответ<br></p>"), "text/html").body;
    const caret = paragraph.querySelector("p > .caret");
    expect(caret).not.toBeNull();
    expect(caret?.nextSibling?.nodeName).toBe("BR");
  });

  it("puts_the_caret_inside_a_loose_list_paragraph", () => {
    const list = new DOMParser().parseFromString(
      caretInTail("<ul>\n<li><p>первый</p></li>\n<li><p>второй</p></li>\n</ul>\n"),
      "text/html",
    ).body;
    const caret = list.querySelector("li:last-child p > .caret");
    expect(caret).not.toBeNull();
    expect(caret?.nextSibling).toBeNull();
  });

  it("does_not_mark_up_empty_html", () => {
    expect(caretInTail("")).toBe("");
  });
});
