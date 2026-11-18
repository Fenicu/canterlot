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
