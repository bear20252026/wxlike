import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ACTIONS } from "../../shell/core-contract";
import { createTestPlatform, loginAs, openConversation, openSettingsTab, renderApp } from "../../test/helpers";

describe("G10-05 设置持久化与本地安全", () => {
  it("主题与紧凑模式落盘，重新打开应用后仍生效", async () => {
    const { fixture, unmount } = renderApp();
    await loginAs();
    const user = userEvent.setup();
    await user.click(screen.getByTestId("rail-settings"));

    await user.click(await screen.findByTestId("theme-dark"));
    await waitFor(() => expect(document.documentElement.dataset.theme).toBe("dark"));

    const compact = screen.getByRole("switch", { name: "紧凑模式" });
    await user.click(compact);
    await waitFor(() => expect(document.documentElement.dataset.compact).toBe("1"));

    unmount();
    renderApp({ fixture, hash: "#/settings" });
    await waitFor(() => expect(screen.getByRole("switch", { name: "紧凑模式" })).toHaveAttribute("aria-checked", "true"));
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(fixture.currentSession().phase).toBe("authenticated");
  });

  it("关闭 Enter 发送后仅 Ctrl/⌘+Enter 发送", async () => {
    const { requests } = renderApp({ record: true });
    await loginAs();
    const user = userEvent.setup();
    await user.click(screen.getByTestId("rail-settings"));
    const toggle = await screen.findByRole("switch", { name: "Enter 发送消息" });
    expect(toggle).toHaveAttribute("aria-checked", "true");
    await user.click(toggle);
    await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "false"));

    await user.click(screen.getByTestId("rail-chats"));
    await openConversation();
    const input = screen.getByTestId("composer-input");
    fireEvent.change(input, { target: { value: "快捷键发送" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(requests.some((item) => item.action_id === ACTIONS.messageSend)).toBe(false);

    fireEvent.keyDown(input, { key: "Enter", ctrlKey: true });
    await within(screen.getByTestId("message-scroller")).findByText("快捷键发送");
    await waitFor(() => {
      const row = within(screen.getByTestId("message-scroller")).getByText("快捷键发送").closest("[data-state]");
      expect(row).toHaveAttribute("data-state", "DEVICE_STORED");
    });
  });

  it("口令迁移先在本地校验，成功提交一次且输入框清空", async () => {
    const { requests } = renderApp({ record: true });
    await loginAs();
    const user = userEvent.setup();
    await user.click(screen.getByTestId("rail-settings"));
    await openSettingsTab("security");

    await user.click(await screen.findByTestId("secrets-migrate"));
    expect(await screen.findByText("请输入当前口令")).toBeInTheDocument();
    expect(screen.getByText("新口令至少 8 位")).toBeInTheDocument();

    await user.type(screen.getByTestId("secrets-current"), "demo1234");
    await user.type(screen.getByTestId("secrets-next"), "newpass123");
    await user.type(screen.getByTestId("secrets-confirm"), "different123");
    await user.click(screen.getByTestId("secrets-migrate"));
    expect(await screen.findByText("两次输入的新口令不一致")).toBeInTheDocument();
    expect(requests.some((item) => item.action_id === ACTIONS.secretsMigrate)).toBe(false);

    await user.clear(screen.getByTestId("secrets-confirm"));
    await user.type(screen.getByTestId("secrets-confirm"), "newpass123");
    await user.click(screen.getByTestId("secrets-migrate"));

    await waitFor(() => expect(requests.filter((item) => item.action_id === ACTIONS.secretsMigrate)).toHaveLength(1));
    await waitFor(() => expect(screen.getByTestId("secrets-current")).toHaveValue(""));
    expect(screen.getByTestId("secrets-next")).toHaveValue("");
    expect(JSON.stringify(localStorage)).not.toContain("newpass123");
  });

  it("备份可校验并二次确认导入；保存失败不伪报成功", async () => {
    let backupBytes: Uint8Array | null = null;
    let savedName = "";
    const platform = createTestPlatform({
      overrides: {
        files: {
          open: async () => {
            if (!backupBytes) return { ok: false, code: "CANCELLED", message: "未选择文件" };
            return {
              ok: true,
              value: [{
                name: "wxlike-backup.json",
                size: backupBytes.byteLength,
                type: "application/json",
                lastModified: Date.now(),
                bytes: async () => backupBytes!,
              }],
            };
          },
          save: async ({ suggestedName, bytes }) => {
            savedName = suggestedName;
            expect(bytes.byteLength).toBeGreaterThan(0);
            return { ok: true, value: { via: "dialog" } };
          },
        },
      },
    });
    const { fixture, requests } = renderApp({ platform, record: true });
    await loginAs();
    const user = userEvent.setup();
    await user.click(screen.getByTestId("rail-settings"));
    await openSettingsTab("security");

    await user.click(await screen.findByTestId("backup-create"));
    await waitFor(() => expect(savedName).toMatch(/^wxlike-backup-user-alpha\.json$/));
    expect(requests.filter((item) => item.action_id === ACTIONS.backupCreate).length).toBeGreaterThanOrEqual(1);

    const backup = await fixture.dispatch<{ base64: string }>({
      action_id: ACTIONS.backupCreate,
      request_id: "test-backup",
      schema_version: 1,
      params: {},
    });
    expect(backup.ok).toBe(true);
    if (!backup.ok) return;
    const binary = atob(backup.data.base64);
    backupBytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));

    await user.click(screen.getByTestId("backup-pick"));
    const verify = await screen.findByTestId("backup-verify-result");
    expect(verify).toHaveTextContent("校验通过");

    await user.click(screen.getByTestId("backup-import"));
    await user.click(await screen.findByTestId("confirm-action"));
    await waitFor(() => {
      const imports = requests.filter((item) => item.action_id === ACTIONS.backupImport);
      expect(imports).toHaveLength(1);
      expect(imports[0].params).toMatchObject({ confirmation: "IMPORT" });
    });
    expect(await screen.findByText(/备份已导入/)).toBeInTheDocument();
  });

  it("平台不支持保存时，备份生成结果如实报告未保存", async () => {
    const platform = createTestPlatform({
      overrides: {
        files: {
          open: async () => ({ ok: false, code: "CANCELLED", message: "未选择文件" }),
          save: async () => ({ ok: false, code: "UNSUPPORTED", message: "浏览环境没有保存对话框" }),
        },
      },
    });
    renderApp({ platform });
    await loginAs();
    const user = userEvent.setup();
    await user.click(screen.getByTestId("rail-settings"));
    await openSettingsTab("security");

    await user.click(await screen.findByTestId("backup-create"));
    expect(await screen.findByText(/备份已生成但未保存/)).toBeInTheDocument();
  });
});
