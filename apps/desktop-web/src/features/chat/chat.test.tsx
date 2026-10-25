import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ACTIONS } from "../../shell/core-contract";
import type { PickedFile } from "../../shell/platform/port";
import { createTestPlatform, loginAs, openConversation, renderApp } from "../../test/helpers";

function messageRow(text: string): HTMLElement {
  const scroller = screen.getByTestId("message-scroller");
  const node = within(scroller).getByText(text);
  const row = node.closest<HTMLElement>("[data-state]");
  if (!row) throw new Error(`message row for ${text} not found`);
  return row;
}

describe("G10-02 聊天闭环", () => {
  it("发送文本经过 LOCAL_QUEUED→DEVICE_STORED 的核心投影，并带幂等键", async () => {
    const { requests } = renderApp({ record: true });
    await loginAs();
    await openConversation();
    const user = userEvent.setup();

    expect(screen.getByTestId("conv-virtual-window")).toBeInTheDocument();
    await user.type(screen.getByTestId("composer-input"), "端到端测试消息");
    await user.keyboard("{Enter}");

    await within(screen.getByTestId("message-scroller")).findByText("端到端测试消息");
    await waitFor(() => expect(messageRow("端到端测试消息")).toHaveAttribute("data-state", "DEVICE_STORED"));

    const sends = requests.filter((item) => item.action_id === ACTIONS.messageSend);
    expect(sends).toHaveLength(1);
    expect(sends[0].idempotency_key).toBeTruthy();
    expect(screen.getByTestId("composer-input")).toHaveValue("");
  });

  it("连续 Enter 只发送一条消息", async () => {
    const { requests } = renderApp({ record: true });
    await loginAs();
    await openConversation();
    const input = screen.getByTestId("composer-input");

    fireEvent.change(input, { target: { value: "只发一次" } });
    fireEvent.keyDown(input, { key: "Enter" });
    fireEvent.keyDown(input, { key: "Enter" });

    await within(screen.getByTestId("message-scroller")).findByText("只发一次");
    await waitFor(() => expect(messageRow("只发一次")).toHaveAttribute("data-state", "DEVICE_STORED"));
    expect(requests.filter((item) => item.action_id === ACTIONS.messageSend)).toHaveLength(1);
  });

  it("草稿由核心保存，切换会话后恢复", async () => {
    const { requests } = renderApp({ record: true });
    await loginAs();
    await openConversation();
    const user = userEvent.setup();

    await user.type(screen.getByTestId("composer-input"), "这是一段草稿");
    await waitFor(() => expect(requests.some((item) => item.action_id === ACTIONS.draftSave)).toBe(true), {
      timeout: 2000,
    });

    await user.click(screen.getByTestId("conv-conv-spring"));
    await screen.findByTestId("composer-input");
    await user.click(screen.getByTestId("conv-conv-lin"));

    await waitFor(() => expect(screen.getByTestId("composer-input")).toHaveValue("这是一段草稿"));
  });

  it("断网发送进入 FAILED_RETRYABLE，恢复后重试等待核心重新投影", async () => {
    const { fixture } = renderApp();
    await loginAs();
    await openConversation();
    const user = userEvent.setup();

    fixture.setOffline(true);
    await user.type(screen.getByTestId("composer-input"), "断网消息");
    await user.keyboard("{Enter}");
    await within(screen.getByTestId("message-scroller")).findByText("断网消息");
    await waitFor(() => expect(messageRow("断网消息")).toHaveAttribute("data-state", "FAILED_RETRYABLE"));

    fixture.setOffline(false);
    const retry = messageRow("断网消息").querySelector<HTMLElement>('[data-testid^="msg-retry-"]');
    expect(retry).not.toBeNull();
    await user.click(retry!);

    await waitFor(() => expect(messageRow("断网消息")).toHaveAttribute("data-state", "DEVICE_STORED"), {
      timeout: 2000,
    });
  });

  it("超过 1GB 的文件在开始上传前被拒绝", async () => {
    const tooLarge: PickedFile = {
      name: "huge.bin",
      size: 1024 * 1024 * 1024 + 1,
      type: "application/octet-stream",
      lastModified: Date.now(),
      bytes: async () => {
        throw new Error("界面不应读取超大文件");
      },
    };
    const platform = createTestPlatform({
      overrides: {
        files: {
          open: async () => ({ ok: true, value: [tooLarge] }),
          save: async () => ({ ok: true, value: { via: "dialog" } }),
        },
      },
    });
    const { requests } = renderApp({ platform, record: true });
    await loginAs();
    await openConversation();
    const user = userEvent.setup();

    await user.click(screen.getByTestId("composer-file"));
    expect(await screen.findByText(/超出 1GB 上传预算/)).toBeInTheDocument();
    expect(requests.some((item) => item.action_id === ACTIONS.mediaUploadBegin)).toBe(false);
  });
});
