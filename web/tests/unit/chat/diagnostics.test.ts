import { describe, expect, it } from "vitest";
import {
  buildDiagnosticsReport,
  type DiagnosticsData,
} from "#lib/screens/chat/diagnostics.js";

describe("chat/diagnostics", () => {
  it("diagnostics_report_contains_no_content_fields", () => {
    const SECRET_PROMPT = "SUPER_SECRET_USER_PROMPT_PASSWORD_123";
    const SECRET_TOOL_ARG = "DROP_DATABASE_SECRET_COMMAND";
    const SECRET_FILENAME = "TOP_SECRET_FINANCIAL_REPORT.XLSX";

    const data: DiagnosticsData = {
      conversationId: "00000000-0000-4000-8000-000000000001",
      connectionId: "conn_test_123",
      execution: {
        id: "00000000-0000-4000-8000-000000000002",
        state: "running",
        generation: 3,
        outcome: "user_stop",
      },
      activity: {
        state: "working",
        evidence: {
          phase: "running_tool",
          tool_name: "bash",
          // Potential leakage in raw evidence:
          prompt: SECRET_PROMPT,
          arguments: SECRET_TOOL_ARG,
          file: SECRET_FILENAME,
        },
      },
      scope: {
        cursor: "journal:14827",
        status: "live",
        resyncCount: 2,
        lastFrameAgeMs: 420,
      },
      versions: {
        build: "2026.10.29",
        protocol: 1,
      },
      diagnosticEvents: [
        {
          kind: "adapter_diagnostic",
          correlation_id: "00000000-0000-4000-8000-000000000003",
          seq: "10",
        },
        {
          kind: "state_divergence",
          correlation_id: "00000000-0000-4000-8000-000000000004",
          seq: "12",
        },
      ],
    };

    const report = buildDiagnosticsReport(data);

    // Must contain identifiers, kinds, cursors, counters
    expect(report).toContain("00000000-0000-4000-8000-000000000001");
    expect(report).toContain("conn_test_123");
    expect(report).toContain("journal:14827");
    expect(report).toContain("running");
    expect(report).toContain("user_stop");
    expect(report).toContain("adapter_diagnostic");
    expect(report).toContain("state_divergence");
    expect(report).toContain("00000000-0000-4000-8000-000000000003");
    expect(report).toContain("resyncCount: 2");

    // MUST NOT contain sensitive contents
    expect(report).not.toContain(SECRET_PROMPT);
    expect(report).not.toContain(SECRET_TOOL_ARG);
    expect(report).not.toContain(SECRET_FILENAME);
  });
});
