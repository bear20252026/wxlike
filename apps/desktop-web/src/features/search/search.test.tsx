import { fireEvent, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ACTIONS, newRequest } from "../../shell/core-contract";
import { loginAs, renderApp } from "../../test/helpers";

describe("G10-05 搜索", () => {
  it("按分类向核心查询，点击结果打开对应会话", async () => {
    renderApp({ hash: "#/chats" });
    await loginAs();
    const user = userEvent.setup();
    await user.click(screen.getByTestId("rail-search"));

    await user.type(await screen.findByTestId("search-input"), "美术馆");
    const hit = await screen.findByTestId("search-result-msg-m-lin-5");
    expect(hit).toHaveTextContent("林小满");
    await user.click(hit);

    await screen.findByTestId("composer-input");
    expect(window.location.hash).toBe("#/chats/conv-lin");
  });

  it("关系变更事件会触发搜索重新查询", async () => {
    const { fixture, requests } = renderApp({ record: true });
    await loginAs();
    const user = userEvent.setup();
    await user.click(screen.getByTestId("rail-search"));

    await user.type(await screen.findByTestId("search-input"), "美术馆");
    await screen.findByTestId("search-result-msg-m-lin-5");
    const before = requests.filter((item) => item.action_id === ACTIONS.searchQuery).length;

    await fixture.dispatch(newRequest(ACTIONS.relationStar, { user_id: "user-lin", star: true }));

    await waitFor(() => {
      const after = requests.filter((item) => item.action_id === ACTIONS.searchQuery).length;
      expect(after).toBeGreaterThan(before);
    });
  });

  it("⌘/Ctrl+K 打开全局搜索，Esc 关闭", async () => {
    renderApp({ hash: "#/chats" });
    await loginAs();
    const user = userEvent.setup();

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    expect(await screen.findByTestId("search-overlay")).toBeInTheDocument();

    await user.type(screen.getByTestId("search-overlay-input"), "林小满");
    expect(await screen.findByTestId("search-hit-contact-user-lin")).toBeInTheDocument();

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByTestId("search-overlay")).not.toBeInTheDocument());
  });

  it("内容搜索未接入时明确说明，不返回静态结果", async () => {
    renderApp({ hash: "#/chats" });
    await loginAs();
    const user = userEvent.setup();
    await user.click(screen.getByTestId("rail-search"));

    await user.type(await screen.findByTestId("search-input"), "美术馆");
    await user.click(await screen.findByTestId("search-category-content"));
    expect(await screen.findByTestId("search-unavailable-content")).toHaveTextContent("尚未接入");
  });
});
