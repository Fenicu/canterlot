// @vitest-environment node
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import type { Browser, Page } from "@playwright/test";
import { scopedSheet } from "#lib/render/mermaid.js";

// jsdom не знает ни constructable stylesheets, ни @scope, ни вложенных правил: граница стилей
// диаграммы проверяется в настоящем Chromium из набора Playwright (docs/development.md,
// «Разовая проверка экрана в браузере»). Где браузер не установлен, проверка пропускается.
const local = join(homedir(), ".cache", "canterlot-playwright");
if (!process.env.PLAYWRIGHT_BROWSERS_PATH && existsSync(local)) process.env.PLAYWRIGHT_BROWSERS_PATH = local;
const { chromium } = await import("@playwright/test");
const installed = existsSync(chromium.executablePath());

const PAGE = `<!doctype html><html><head><style>
  @keyframes spin { to { rotate: 360deg } }
  #busy { animation: spin 1s linear infinite }
</style></head><body>
  <button id="deny">Отклонить</button>
  <p id="cmd">rm -rf ~</p>
  <i id="busy"></i>
  <div class="mermaid-view" id="mermaid-view-0"><svg id="mermaid-0" viewBox="0 0 10 10" width="10" height="10">
    <g class="node mermaid-inline-0"><path class="edge" d="M0 0L1 1"/></g></svg></div>
  <div class="mermaid-view" id="mermaid-view-1"><svg id="mermaid-0" viewBox="0 0 10 10" width="10" height="10">
    <g class="node"><path class="edge" d="M0 0L1 1"/></g></svg></div>
</body></html>`;

/** Правило из отчёта ревью, как его выпускал extractSvgCss, и другие способы выйти за диаграмму. */
const HOSTILE = [
  "#mermaid-0 .mermaid-inline-0{fill:rgb(255, 0, 0);path:not(a){body:has(&) button{display:none}}}",
  "body button{display:none}",
  "html{display:none}",
  "*{visibility:hidden}",
  ":scope{display:none}",
  ":scope ~ div, :scope + div{display:none}",
  "body:has(:scope) #cmd{color:rgb(255, 0, 0)}",
  "&{display:none}",
  "@scope (body){button{display:none}}",
  "@media all{button{display:none}}",
  "@keyframes spin{to{opacity:0}}",
  "@keyframes dash{to{stroke-dashoffset:0}}",
  "#mermaid-0 .edge{animation:dash 5s linear infinite;stroke:rgb(0, 0, 255)}",
].join("\n");

interface Seen {
  deny: string;
  denyVisibility: string;
  html: string;
  cmd: string;
  view: string;
  neighbour: string;
  nodeFill: string;
  neighbourFill: string;
  edgeStroke: string;
  edgeVisibility: string;
  edgeAnimation: string;
  busyAnimation: string;
  keyframes: string[];
  topLevel: string[];
}

async function look(page: Page, mode: "raw" | "scoped"): Promise<Seen> {
  return page.evaluate(
    ({ css, mode }) => {
      const build = (window as unknown as { scopedSheet: typeof scopedSheet }).scopedSheet;
      let sheet: CSSStyleSheet;
      if (mode === "scoped") sheet = build("mermaid-view-0", css);
      else {
        // Так таблица вставала до исправления: сырой текст на весь документ.
        sheet = new CSSStyleSheet();
        sheet.replaceSync(css);
      }
      document.adoptedStyleSheets = [sheet];
      const style = (selector: string): CSSStyleDeclaration => getComputedStyle(document.querySelector(selector)!);
      const seen = {
        deny: style("#deny").display,
        denyVisibility: style("#deny").visibility,
        html: style("html").display,
        cmd: style("#cmd").color,
        view: style("#mermaid-view-0").display,
        neighbour: style("#mermaid-view-1").display,
        nodeFill: style("#mermaid-view-0 .node").fill,
        neighbourFill: style("#mermaid-view-1 .node").fill,
        edgeStroke: style("#mermaid-view-0 .edge").stroke,
        edgeVisibility: style("#mermaid-view-0 .edge").visibility,
        edgeAnimation: style("#mermaid-view-0 .edge").animationName,
        busyAnimation: style("#busy").animationName,
        keyframes: Array.from(sheet.cssRules).filter((rule) => rule instanceof CSSKeyframesRule).map((rule) => (rule as CSSKeyframesRule).name),
        topLevel: Array.from(sheet.cssRules).map((rule) => rule.constructor.name),
      };
      document.adoptedStyleSheets = [];
      return seen;
    },
    { css: HOSTILE, mode },
  );
}

describe.skipIf(!installed)("стили диаграммы mermaid в Chromium (нужен браузер Playwright)", () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await chromium.launch({ headless: true });
    page = await browser.newPage();
    await page.setContent(PAGE);
    // В страницу уходит текст той самой функции, что работает в приложении.
    await page.addScriptTag({ content: `window.scopedSheet = ${scopedSheet.toString()};` });
    // Запуск браузера на занятой машине дольше обычного предела для хуков.
  }, 120_000);

  afterAll(async () => {
    await browser?.close();
  });

  it("сырая таблица, как до исправления, скрывает кнопки приложения", async () => {
    const seen = await look(page, "raw");
    expect(seen.deny).toBe("none");
    expect(seen.html).toBe("none");
  });

  it("таблица в границе контейнера не трогает ничего за пределами диаграммы", async () => {
    const seen = await look(page, "scoped");
    expect(seen.deny).toBe("inline-block");
    expect(seen.denyVisibility).toBe("visible");
    expect(seen.html).toBe("block");
    expect(seen.cmd).toBe("rgb(0, 0, 0)");
    // Сам контейнер и соседняя диаграмма с тем же id у svg тоже вне досягаемости.
    expect(seen.view).toBe("block");
    expect(seen.neighbour).toBe("block");
    expect(seen.neighbourFill).not.toBe("rgb(255, 0, 0)");
    // Анимации приложения не подменяются: кадры диаграммы живут под своим именем.
    expect(seen.busyAnimation).toBe("spin");
    expect(seen.keyframes).toEqual(["mermaid-view-0-spin", "mermaid-view-0-dash"]);
    expect(seen.topLevel).toEqual(["CSSScopeRule", "CSSKeyframesRule", "CSSKeyframesRule"]);
  });

  it("внутри диаграммы её стили работают", async () => {
    const seen = await look(page, "scoped");
    expect(seen.nodeFill).toBe("rgb(255, 0, 0)");
    expect(seen.edgeStroke).toBe("rgb(0, 0, 255)");
    expect(seen.edgeVisibility).toBe("hidden");
    expect(seen.edgeAnimation).toBe("mermaid-view-0-dash");
  });
});
