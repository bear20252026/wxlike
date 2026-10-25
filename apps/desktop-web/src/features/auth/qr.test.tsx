import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ACTIONS } from "../../shell/core-contract";
import { renderApp } from "../../test/helpers";

function challengeId(): string | null {
  const node = screen.getByTestId("qr-countdown");
  const match = node.textContent?.match(/challenge (\S+)/);
  return match?.[1] ?? null;
}

describe("G10-06 扫码登录状态机", () => {
  it("只展示核心 challenge，扫码后仍停留在等待页", async () => {
    const { fixture } = renderApp({ hash: "#/login/qr" });
    await screen.findByTestId("qr-image");
    expect(screen.getByTestId("qr-state")).toHaveTextContent("等待扫码");

    act(() => fixture.simulateQr("SCANNED"));
    expect(await screen.findByTestId("qr-state")).toHaveTextContent("已扫码");
    expect(fixture.currentSession().phase).toBe("signed_out");
    expect(screen.queryByTestId("rail-chats")).not.toBeInTheDocument();
    expect(screen.queryByTestId("composer-input")).not.toBeInTheDocument();
  });

  it("手机批准后由核心签发会话，桌面才进入账号", async () => {
    const { fixture, requests } = renderApp({ hash: "#/login/qr", record: true });
    await screen.findByTestId("qr-image");

    act(() => fixture.simulateQr("APPROVED"));
    await screen.findByTestId("rail-chats");

    expect(fixture.currentSession().phase).toBe("authenticated");
    expect(requests.some((item) => item.action_id === ACTIONS.qrCreate)).toBe(true);
    expect(requests.some((item) => item.action_id === ACTIONS.login)).toBe(false);
  });

  it("拒绝和过期都不会进入账号", async () => {
    const { fixture } = renderApp({ hash: "#/login/qr" });
    await screen.findByTestId("qr-image");

    act(() => fixture.simulateQr("REJECTED"));
    expect(await screen.findByTestId("qr-state")).toHaveTextContent("登录被拒绝");
    expect(fixture.currentSession().phase).toBe("signed_out");
    expect(screen.queryByTestId("rail-chats")).not.toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByTestId("qr-refresh"));
    await waitFor(() => expect(screen.getByTestId("qr-state")).toHaveTextContent("等待扫码"));

    act(() => fixture.simulateQr("EXPIRED"));
    expect(await screen.findByTestId("qr-state")).toHaveTextContent("二维码已过期");
    expect(fixture.currentSession().phase).toBe("signed_out");
  });

  it("切换账号会撤销旧 challenge 并生成新的 challenge", async () => {
    const user = userEvent.setup();
    const { fixture, requests } = renderApp({ hash: "#/login/qr", record: true });
    await screen.findByTestId("qr-image");
    const first = challengeId();
    expect(first).toBeTruthy();

    await user.click(screen.getByTestId("qr-switch"));

    await waitFor(() => {
      const next = challengeId();
      expect(next).toBeTruthy();
      expect(next).not.toBe(first);
    });
    expect(fixture.currentSession().phase).toBe("signed_out");
    expect(requests.filter((item) => item.action_id === ACTIONS.qrSwitchAccount)).toHaveLength(1);
    expect(requests.filter((item) => item.action_id === ACTIONS.qrCreate).length).toBeGreaterThanOrEqual(2);
  });

  it("生产界面没有桌面确认入口；DEV 模拟控件明确标注非验收能力", async () => {
    renderApp({ hash: "#/login/qr" });
    await screen.findByTestId("qr-image");

    expect(screen.queryByText("确认登录")).not.toBeInTheDocument();
    expect(screen.queryByText("批准登录")).not.toBeInTheDocument();
    expect(screen.getByText(/开发夹具/)).toHaveTextContent("不是产品能力");
  });
});
