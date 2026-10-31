import { describe, expect, it } from "vitest";
import { permissionView } from "#lib/requests/permissionView.js";

describe("requests/permission view of OpenCode", () => {
  it("a_shell_request_shows_the_command", () => {
    const view = permissionView({
      tool_name: "shell",
      command: "echo one > one.txt",
      agent: "orchestrator",
      deny_also_interrupts: false,
    });
    expect(view.command).toBe("echo one > one.txt");
    expect(view.denyInterrupts).toBe(false);
  });

  it("a_read_request_shows_the_file", () => {
    const view = permissionView({
      tool_name: "read",
      input: { file_path: "one.txt" },
    });
    expect(view.facts).toEqual([{ label: "Файл", value: "one.txt" }]);
    expect(view.fields).toEqual([]);
  });

  it("an_edit_request_shows_the_change", () => {
    const view = permissionView({
      tool_name: "edit",
      changes: [{ path: "w.txt", kind: "add", diff: "+w" }],
    });
    expect(view.changes).toEqual([
      { path: "w.txt", kind: "Новый файл", diff: "+w" },
    ]);
  });

  it("an_external_directory_request_shows_the_folder", () => {
    const view = permissionView({
      tool_name: "external_directory",
      grant_root: "/etc/*",
    });
    expect(view.facts).toEqual([{ label: "Доступ к папке", value: "/etc/*" }]);
  });
});
