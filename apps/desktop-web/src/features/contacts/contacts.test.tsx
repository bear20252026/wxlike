import { fireEvent, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ACTIONS, newRequest } from "../../shell/core-contract";
import { loginAs, renderApp } from "../../test/helpers";

describe("G10-03 联系人与群管理", () => {
  it("接受好友申请只提交一次，接受后双方关系进入通讯录", async () => {
    const { requests } = renderApp({ record: true });
    await loginAs();
    const user = userEvent.setup();

    await user.click(screen.getByTestId("rail-new_friends"));
    const accept = await screen.findByTestId("friend-accept-req-alpha-1");
    fireEvent.click(accept);
    fireEvent.click(accept);

    expect(await screen.findByText("已接受")).toBeInTheDocument();
    expect(requests.filter((item) => item.action_id === ACTIONS.requestAccept)).toHaveLength(1);

    await user.click(await screen.findByTestId("friend-open-req-alpha-1"));
    expect(await screen.findByTestId("contact-detail")).toBeInTheDocument();

    await user.click(screen.getByTestId("rail-contacts"));
    expect(await screen.findByTestId("contact-user-tang")).toBeInTheDocument();
  });

  it("备注保存到核心关系链，重新打开仍存在", async () => {
    const { fixture, unmount } = renderApp({ hash: "#/chats" });
    await loginAs();
    const user = userEvent.setup();

    window.location.hash = "#/contacts/user-lin";
    await screen.findByTestId("contact-detail");
    const note = await screen.findByTestId("contact-note");
    await user.clear(note);
    await user.type(note, "新的备注名");
    await user.click(screen.getByTestId("contact-note-save"));
    expect(await screen.findByText("备注已保存")).toBeInTheDocument();

    unmount();
    renderApp({ fixture, hash: "#/contacts/user-lin" });
    await waitFor(() => expect(screen.getByTestId("contact-note")).toHaveValue("新的备注名"));
  });

  it("群主可以移除成员，普通成员看不到移除入口且服务端仍拒绝", async () => {
    const { fixture } = renderApp({ hash: "#/chats" });
    await loginAs();
    const user = userEvent.setup();

    window.location.hash = "#/groups/group-spring";
    await screen.findByTestId("group-page");
    expect(await screen.findByTestId("group-kick-user-anna")).toBeInTheDocument();
    await user.click(screen.getByTestId("group-kick-user-anna"));
    await user.click(await screen.findByTestId("confirm-action"));
    await waitFor(() => expect(screen.queryByTestId("group-member-user-anna")).not.toBeInTheDocument());

    window.location.hash = "#/groups/group-weekend";
    expect(await screen.findByText(/只有群主或管理员可以改名、移除成员/)).toBeInTheDocument();
    expect(document.querySelector('[data-testid^="group-kick-"]')).toBeNull();

    const denied = await fixture.dispatch(
      newRequest(ACTIONS.groupKick, { group_id: "group-weekend", user_id: "user-ge" }),
    );
    expect(denied.ok).toBe(false);
    if (!denied.ok) expect(denied.error.code).toBe("FORBIDDEN");
  });
});
