import { describe, expect, it } from "vitest";
import {
  extractSvgCss,
  mermaidPhase,
  mermaidThemeVariables,
  sanitizeSvg
} from "#lib/render/mermaid.js";

describe("mermaidPhase", () => {
  const base = { streaming: false, failed: false, ready: false, mode: "diagram" as const };

  it("streaming block stays a code block whatever the later flags say", () => {
    expect(mermaidPhase({ ...base, streaming: true, mode: "diagram", ready: true })).toBe("streaming");
    expect(mermaidPhase({ ...base, streaming: true, failed: true })).toBe("streaming");
  });

  it("complete block in diagram mode waits for the library as loading", () => {
    expect(mermaidPhase(base)).toBe("loading");
  });

  it("complete block with a rendered svg shows the diagram", () => {
    expect(mermaidPhase({ ...base, ready: true })).toBe("diagram");
  });

  it("a syntax error shows the source and the failure line", () => {
    expect(mermaidPhase({ ...base, ready: true, failed: true })).toBe("error");
    expect(mermaidPhase({ ...base, failed: true })).toBe("error");
  });

  it("the code toggle switches away from the diagram", () => {
    expect(mermaidPhase({ ...base, ready: true, mode: "code" })).toBe("code");
  });
});

describe("mermaidThemeVariables", () => {
  it("mirrors the app tokens into mermaid theme variables", () => {
    const colors: Record<string, string> = {
      "--bg": "#101010",
      "--surface": "#202020",
      "--raised": "#303030",
      "--text": "#e0e0e0",
      "--line": "#404040",
      "--muted": "#9aa0a6",
      "--accent": "#7aa2f7"
    };
    const variables = mermaidThemeVariables((name) => colors[name] ?? "");
    expect(variables.background).toBe("#101010");
    expect(variables.primaryColor).toBe("#202020");
    expect(variables.primaryTextColor).toBe("#e0e0e0");
    expect(variables.primaryBorderColor).toBe("#9aa0a6");
    expect(variables.nodeBorder).toBe("#9aa0a6");
    expect(variables.lineColor).toBe("#9aa0a6");
    expect(variables.clusterBorder).toBe("#9aa0a6");
    expect(variables.secondaryColor).toBe("#303030");
  });

  it("keeps safe fallbacks when tokens are not yet applied", () => {
    const variables = mermaidThemeVariables(() => "");
    expect(variables.background).toBeTruthy();
    expect(variables.primaryTextColor).toBeTruthy();
    expect(variables.lineColor).toBeTruthy();
  });
});

describe("extractSvgCss", () => {
  it("moves the inline style element and style attributes into an external css string", () => {
    const svg =
      '<svg id="m-1" width="100%" viewBox="-8 -8 120 40" style="max-width: 480px">' +
      '<style>#m-1 .label{fill:#111}</style>' +
      '<rect x="1" y="1" width="2" height="2" style="fill:red;stroke:blue"/>' +
      '<path d="M0 0 L1 1"/></svg>';
    const { markup, css, natural } = extractSvgCss(svg);
    expect(natural).toEqual({ width: 480, height: 40 });
    expect(markup).toContain('width="480"');
    expect(markup).toContain('height="40"');
    expect(markup).not.toContain('width="100%"');
    expect(markup).not.toContain("<style");
    expect(markup).not.toContain("style=");
    expect(css).toContain("#m-1 .label{fill:#111}");
    expect(css).toContain("fill:red");
    expect(markup).toContain("mermaid-inline-0");
    expect(markup).toContain('<path d="M0 0 L1 1"/>');
  });

  it("keeps only plain declarations of a style attribute", () => {
    const hostile = [
      "path:not(a){body:has(&amp;) button{display:none}}",
      "} body button{display:none",
      "@import url(x)",
      "color:r\\65 d",
      "font-family:&quot;a",
      "fill:url(#g"
    ];
    const svg =
      '<svg id="m-1" viewBox="0 0 10 10">' +
      `<g style="fill:red;${hostile.join(";")};stroke-width: 2px"><path d="M0 0 L1 1"/></g>` +
      '<text style="font-family:&quot;trebuchet ms&quot;, verdana;fill:rgb(1, 2, 3);stroke:url(#g)">x</text></svg>';
    const { css } = extractSvgCss(svg);
    expect(css.split("\n")).toEqual([
      "#m-1 .mermaid-inline-0{fill:red;stroke-width: 2px}",
      '#m-1 .mermaid-inline-1{font-family:"trebuchet ms", verdana;fill:rgb(1, 2, 3);stroke:url(#g)}'
    ]);
  });

  it("a classDiagram style line cannot smuggle a rule through real mermaid output", async () => {
    // jsdom не умеет геометрию SVG: mermaid достаточно любых размеров.
    const proto = (globalThis as unknown as { SVGElement: { prototype: Record<string, unknown> } })
      .SVGElement.prototype;
    proto.getBBox = () => ({ x: 0, y: 0, width: 80, height: 20 });
    proto.getComputedTextLength = () => 80;
    const mermaid = (await import("mermaid")).default;
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      htmlLabels: false,
      suppressErrorRendering: true,
      theme: "base"
    });
    const { svg } = await mermaid.render(
      "mermaid-0",
      "classDiagram\n class A\n style A fill:red,path:not(a){body:has(&) button{display:none}}"
    );
    // Сам mermaid в режиме strict значение не чистит: защита — на нашей стороне.
    expect(svg).toMatch(/style="[^"]*body:has/);
    const { css } = extractSvgCss(svg);
    expect(css).not.toContain("body:has");
    expect(css).toMatch(/\.mermaid-inline-\d+\{fill:red\}/);
  });
});

describe("sanitizeSvg", () => {
  it("keeps diagram geometry and drops scripts and event handlers", () => {
    const svg =
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">' +
      '<g><path d="M0 0 L1 1"/><text>ok</text></g>' +
      '<script>alert(1)</script><g onclick="alert(1)"><circle r="1"/></g></svg>';
    const clean = sanitizeSvg(svg);
    expect(clean).toContain("<svg");
    expect(clean).toContain("M0 0 L1 1");
    expect(clean).not.toContain("script");
    expect(clean).not.toContain("onclick");
  });
});
