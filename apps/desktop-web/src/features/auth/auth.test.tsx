import { fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ACTIONS } from "../../shell/core-contract";
import { loginAs, renderApp } from "../../test/helpers";

describe("G10-01 登录、锁定与恢复路由", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("空表单给出字段错误，错误口令由核心返回且不进入账号", async () => {
    const user = userEvent.setup();
    renderApp();
    await screen.findByTestId("login-username");

    await user.click(screen.getByTestId("login-submit"));
    expect(await screen.findByText("请输入账号")).toBeInTheDocument();
    expect(screen.getByText("请输入口令")).toBeInTheDocument();

    await user.type(screen.getByTestId("login-username"), "user-alpha");
    await user.type(screen.getByTestId("login-password"), "wrong-password");
    await user.click(screen.getByTestId("login-submit"));

    expect(await screen.findByText("账号或口令不正确")).toBeInTheDocument();
    expect(screen.queryByTestId("rail-chats")).not.toBeInTheDocument();
    expect(screen.getByTestId("login-password")).toHaveValue("");
  });

  it("登录成功后进入聊天，且口令不写入 localStorage", async () => {
    renderApp();
    await loginAs();

    expect(screen.getByTestId("rail-chats")).toBeInTheDocument();
    expect(screen.getByTestId("conv-conv-lin")).toBeInTheDocument();
    const stored = Object.entries(localStorage).map(([key, value]) => `${key}=${value}`).join("|");
    expect(stored).not.toContain("demo1234");
  });

  it("重复提交被锁定，只向核心发送一次登录请求", async () => {
    const { requests } = renderApp({ record: true });
    await screen.findByTestId("login-username");
    const user = userEvent.setup();
    await user.type(screen.getByTestId("login-username"), "user-alpha");
    await user.type(screen.getByTestId("login-password"), "demo1234");

    const form = screen.getByTestId("login-submit").closest("form");
    expect(form).not.toBeNull();
    fireEvent.submit(form!);
    fireEvent.submit(form!);
    await screen.findByTestId("rail-chats");

    expect(requests.filter((item) => item.action_id === ACTIONS.login)).toHaveLength(1);
  });

  it("锁定后必须重新验证口令，错误口令不解锁", async () => {
    renderApp();
    await loginAs();
    const user = userEvent.setup();

    await user.click(screen.getByTestId("rail-lock"));
    await screen.findByTestId("unlock-password");

    await user.type(screen.getByTestId("unlock-password"), "not-the-password");
    await user.click(screen.getByTestId("unlock-submit"));
    expect(await screen.findByText("口令不正确")).toBeInTheDocument();
    expect(screen.getByTestId("unlock-password")).toBeInTheDocument();

    await user.type(screen.getByTestId("unlock-password"), "demo1234");
    await user.click(screen.getByTestId("unlock-submit"));
    await screen.findByTestId("rail-chats");
  });

  it("注销由核心撤销会话并回到登录页，之后刷新仍是未登录", async () => {
    const { fixture, unmount } = renderApp();
    await loginAs();
    const user = userEvent.setup();

    await user.click(screen.getByTestId("rail-settings"));
    await user.click(await screen.findByTestId("settings-logout"));
    await user.click(await screen.findByTestId("confirm-action"));
    await screen.findByTestId("login-username");

    expect(fixture.currentSession().phase).toBe("signed_out");
    unmount();

    renderApp({ fixture, hash: "#/chats" });
    await screen.findByTestId("login-username");
  });

  it("已登录会话在新窗口打开时直接按投影进入聊天", async () => {
    const { fixture, unmount } = renderApp();
    await loginAs();
    unmount();

    renderApp({ fixture, hash: "#/chats/conv-lin" });
    await screen.findByTestId("composer-input");
    expect(fixture.currentSession().phase).toBe("authenticated");
  });

  it("没有可恢复会话时恢复页明确失败，不伪造登录", async () => {
    renderApp({ hash: "#/restore" });
    await screen.findByTestId("restore-submit");
    const user = userEvent.setup();
    await user.click(screen.getByTestId("restore-submit"));

    expect(await screen.findByText("没有可恢复的会话")).toBeInTheDocument();
    expect(screen.getByTestId("restore-to-login")).toBeInTheDocument();
    expect(screen.queryByTestId("composer-input")).not.toBeInTheDocument();
  });
});

describe("G10-01 登录路由守卫", () => {
  it("未登录访问聊天会落到登录页", async () => {
    renderApp({ hash: "#/chats/conv-lin" });
    await screen.findByTestId("login-username");
    expect(screen.queryByTestId("composer-input")).not.toBeInTheDocument();
  });

  it("已登录访问登录路由会被送回聊天", async () => {
    const { fixture } = renderApp();
    await loginAs();

    window.location.hash = "#/login";
    await screen.findByTestId("rail-chats");
    expect(screen.queryByTestId("login-submit")).not.toBeInTheDocument();
    expect(fixture.currentSession().phase).toBe("authenticated");
    expect(ACTIONS.login).toBe("identity.session.login");
  });
});
