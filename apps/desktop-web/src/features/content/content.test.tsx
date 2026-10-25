/**
 * G11-01 test matrix — success / empty / param error / unauthorized /
 * timeout-network / cancel / duplicate submit / XSS / cross-account
 * visibility / three-dot menus / draft persistence / favorites actions.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { setPlatform, type PlatformPort } from "../../lib/platform";
import { setTransport } from "../../lib/http";
import { err, fixtureTransport, loggingTransport, type CallLog } from "../../test/fixture";
import { ContentApi } from "./api";
import { ArticlesPage } from "./ArticlesPage";
import { FavoritesPage } from "./FavoritesPage";
import { MomentsPage } from "./MomentsPage";
import { StickerPanel } from "./StickerPanel";
import type { ContentPorts } from "./model";

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  cleanup();
  setTransport(null);
});

const now = 1_700_000_000_000;

function moment(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    moment_id: "m1",
    author: "user-a",
    author_name: "小明",
    text: "第一条动态",
    media_ids: [],
    visibility: "friends",
    created_at: now,
    like_count: 2,
    comment_count: 1,
    ...overrides,
  };
}

async function renderMoments(
  routes: Record<string, (method: string, path: string, body: unknown) => unknown>,
  log?: CallLog,
): Promise<ContentPorts> {
  const api = new ContentApi();
  setTransport(log ? loggingTransport(routes as never, log) : fixtureTransport(routes as never));
  render(<MomentsPage ports={api} />);
  return api;
}

describe("MomentsPage — feed", () => {
  it("成功：加载 feed 并渲染动态", async () => {
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [moment()], next_cursor: null }),
    });
    await waitFor(() => expect(screen.getByTestId("moment-m1")).toBeInTheDocument());
    expect(screen.getByTestId("moment-text")).toHaveTextContent("第一条动态");
  });

  it("空结果：显示空状态", async () => {
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [], next_cursor: null }),
    });
    await waitFor(() => expect(screen.getByText("还没有动态")).toBeInTheDocument());
  });

  it("断网：显示网络错误横幅", async () => {
    await renderMoments({
      "GET /api/moments/feed": () => err(0, "fetch failed", "network"),
    });
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("网络不可用"));
  });

  it("未授权：401 给出重新登录提示", async () => {
    await renderMoments({
      "GET /api/moments/feed": () => err(401, "token 过期"),
    });
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("登录已过期"));
  });
});

describe("MomentsPage — publish", () => {
  it("成功：发布后清空草稿并前置到 feed", async () => {
    const published = moment({ moment_id: "m-new", author: "me", text: "新动态" });
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [], next_cursor: null }),
      "POST /api/moments": (_m, _p, body) => {
        expect((body as Record<string, unknown>).text).toBe("新动态");
        expect((body as Record<string, unknown>).visibility).toBe("friends");
        return published;
      },
    });
    fireEvent.change(screen.getByTestId("composer-text"), { target: { value: "新动态" } });
    fireEvent.click(screen.getByTitle("发布"));
    await waitFor(() => expect(screen.getByTestId("moment-m-new")).toBeInTheDocument());
    expect(screen.getByTestId("composer-text")).toHaveValue("");
    expect(localStorage.getItem("wxlike-draft:moments-composer")).toContain('""');
  });

  it("参数错误：空文本时发布按钮禁用", async () => {
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [], next_cursor: null }),
    });
    const button = screen.getByTitle("发布") as HTMLButtonElement;
    expect(button).toBeDisabled();
  });

  it("保存错误保留草稿：服务端 422 后文本仍在，草稿未清", async () => {
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [], next_cursor: null }),
      "POST /api/moments": () => err(422, "文本超出长度限制"),
    });
    fireEvent.change(screen.getByTestId("composer-text"), { target: { value: "会失败的内容" } });
    fireEvent.click(screen.getByTitle("发布"));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("文本超出长度限制"));
    expect(screen.getByTestId("composer-text")).toHaveValue("会失败的内容");
    expect(localStorage.getItem("wxlike-draft:moments-composer")).toContain("会失败的内容");
  });

  it("取消：发布中断后不出现成功状态", async () => {
    let release: ((value: unknown) => void) | undefined;
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [], next_cursor: null }),
      "POST /api/moments": () =>
        new Promise((resolve) => {
          release = resolve;
        }),
    });
    fireEvent.change(screen.getByTestId("composer-text"), { target: { value: "稍后取消" } });
    fireEvent.click(screen.getByTitle("发布"));
    const cancel = await screen.findByText("取消");
    fireEvent.click(cancel);
    release?.(undefined);
    await waitFor(() => expect(screen.queryByText("已发布")).not.toBeInTheDocument());
    await waitFor(() => expect(screen.getByTestId("composer-text")).toHaveValue("稍后取消"));
    expect(screen.getByText("已取消")).toBeInTheDocument();
  });

  it("重复提交：双击只发出一次 POST", async () => {
    const calls: string[] = [];
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [], next_cursor: null }),
      "POST /api/moments": () => {
        calls.push("publish");
        return moment({ moment_id: `m-${calls.length}`, author: "me" });
      },
    });
    fireEvent.change(screen.getByTestId("composer-text"), { target: { value: "只发一次" } });
    const button = screen.getByTitle("发布");
    fireEvent.click(button);
    fireEvent.click(button);
    await waitFor(() => expect(screen.getByTestId("moment-m-1")).toBeInTheDocument());
    expect(calls.length).toBe(1);
  });

  it("笔记刷新保留：草稿持久化，重挂载后恢复", async () => {
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [], next_cursor: null }),
    });
    fireEvent.change(screen.getByTestId("composer-text"), { target: { value: "刷新也不能丢" } });
    await waitFor(() =>
      expect(localStorage.getItem("wxlike-draft:moments-composer")).toContain("刷新也不能丢"),
    );
    cleanup();
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [], next_cursor: null }),
    });
    expect(screen.getByTestId("composer-text")).toHaveValue("刷新也不能丢");
  });
});

describe("MomentsPage — interactions", () => {
  it("XSS payload 不执行：脚本标签被净化", async () => {
    await renderMoments({
      "GET /api/moments/feed": () =>
        ({
          items: [
            moment({
              moment_id: "x1",
              text: '<img src=x onerror="window.__pwned=1"><script>window.__pwned=2</script>正常内容',
            }),
          ],
          next_cursor: null,
        }),
    });
    await waitFor(() => expect(screen.getByTestId("moment-x1")).toBeInTheDocument());
    expect((window as unknown as Record<string, unknown>).__pwned).toBeUndefined();
    expect(screen.getByTestId("moment-text").querySelector("script")).toBeNull();
    expect(screen.getByTestId("moment-text").querySelector("img[onerror]")).toBeNull();
    expect(screen.getByTestId("moment-text")).toHaveTextContent("正常内容");
  });

  it("点赞：乐观更新，失败回滚", async () => {
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [moment()], next_cursor: null }),
      "POST /api/moments/:id/likes": () => ({ like_count: 3 }),
    });
    const likeButton = await screen.findByTestId("like-m1");
    fireEvent.click(likeButton);
    await waitFor(() => expect(likeButton).toHaveTextContent("已赞"));
    expect(likeButton).toHaveTextContent("3");
  });

  it("点赞失败回滚到原状态", async () => {
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [moment()], next_cursor: null }),
      "POST /api/moments/:id/likes": () => err(429, "操作过于频繁"),
    });
    const likeButton = await screen.findByTestId("like-m1");
    fireEvent.click(likeButton);
    await waitFor(() => expect(likeButton).toHaveTextContent("赞 · 2"));
    expect(screen.getByRole("alert")).toHaveTextContent("操作过于频繁");
  });

  it("重复提交点赞：双击只发一次", async () => {
    const calls: string[] = [];
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [moment()], next_cursor: null }),
      "POST /api/moments/:id/likes": () => {
        calls.push("like");
        return { like_count: 3 };
      },
    });
    const likeButton = await screen.findByTestId("like-m1");
    fireEvent.click(likeButton);
    fireEvent.click(likeButton);
    await waitFor(() => expect(likeButton).toHaveTextContent("已赞"));
    expect(calls.length).toBe(1);
  });

  it("跨账号：别人的动态只有举报，没有删除", async () => {
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [moment({ author: "user-b", author_name: "路人" })], next_cursor: null }),
    });
    await waitFor(() => expect(screen.getByTestId("moment-m1")).toBeInTheDocument());
    fireEvent.click(screen.getByTestId("dot-menu-trigger"));
    const menu = screen.getByRole("menu");
    expect(menu).toHaveTextContent("举报");
    expect(menu).not.toHaveTextContent("删除");
  });

  it("自己的动态：菜单有删除，删除后从 feed 移除", async () => {
    localStorage.setItem("wxlike-web-user", "user-a");
    let deleted = false;
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [moment({ author: "user-a" })], next_cursor: null }),
      "DELETE /api/moments/:id": () => {
        deleted = true;
        return null;
      },
    });
    await waitFor(() => expect(screen.getByTestId("moment-m1")).toBeInTheDocument());
    fireEvent.click(screen.getByTestId("dot-menu-trigger"));
    fireEvent.click(screen.getByRole("menuitem", { name: "删除" }));
    await waitFor(() => expect(deleted).toBe(true));
    await waitFor(() => expect(screen.queryByTestId("moment-m1")).not.toBeInTheDocument());
  });

  it("三点菜单：评论菜单可展开并包含举报评论项", async () => {
    localStorage.setItem("wxlike-web-user", "user-c");
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [moment({ author: "user-a" })], next_cursor: null }),
      "GET /api/moments/:id": () => ({
        ...moment(),
        comments: [{ comment_id: "c1", author: "user-a", text: "评论内容", reply_to_comment_id: null, created_at: now }],
      }),
    });
    await waitFor(() => expect(screen.getByTestId("moment-m1")).toBeInTheDocument());
    fireEvent.click(screen.getByTestId("comments-toggle-m1"));
    await waitFor(() => expect(screen.getByText(/评论内容/)).toBeInTheDocument());
    const triggers = screen.getAllByTestId("dot-menu-trigger");
    fireEvent.click(triggers[triggers.length - 1] as Element);
    expect(screen.getByRole("menu")).toHaveTextContent("举报评论");
  });

  it("评论：发送成功后追加并清空输入", async () => {
    localStorage.setItem("wxlike-web-user", "user-me");
    await renderMoments({
      "GET /api/moments/feed": () => ({ items: [moment({ author: "user-a" })], next_cursor: null }),
      "GET /api/moments/:id": () => ({ ...moment(), comments: [] }),
      "POST /api/moments/:id/comments": (_m, _p, body) => ({
        comment_id: "c9",
        author: "user-me",
        text: (body as Record<string, unknown>).text as string,
        reply_to_comment_id: null,
        created_at: now,
      }),
    });
    await waitFor(() => expect(screen.getByTestId("moment-m1")).toBeInTheDocument());
    fireEvent.click(screen.getByTestId("comments-toggle-m1"));
    fireEvent.change(await screen.findByTestId("comment-input-m1"), { target: { value: "好文" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(screen.getByText(/好文/)).toBeInTheDocument());
    expect(screen.getByTestId("comment-input-m1")).toHaveValue("");
  });
});

describe("FavoritesPage", () => {
  const favorites = [
    { id: "f1", user_id: "me", source_conv: "c1", source_msg_id: "msg1", kind: "text", text: "会议纪要", media_id: "", sender: "小明", sent_at_ms: now, created_at: now },
    { id: "f2", user_id: "me", source_conv: "c2", source_msg_id: "msg2", kind: "image", text: "设计稿截图", media_id: "media-1", sender: "小红", sent_at_ms: now - 1000, created_at: now - 1000 },
  ];

  it("成功加载并支持搜索/类型/排序", async () => {
    setTransport(
      (await import("../../test/fixture")).fixtureTransport({
        "GET /api/favorites": () => favorites,
      }),
    );
    render(<FavoritesPage ports={new ContentApi()} />);
    await waitFor(() => expect(screen.getByTestId("favorite-f1")).toBeInTheDocument());
    fireEvent.change(screen.getByTestId("favorites-search"), { target: { value: "会议" } });
    expect(screen.getByTestId("favorite-f1")).toBeInTheDocument();
    expect(screen.queryByTestId("favorite-f2")).not.toBeInTheDocument();
    fireEvent.change(screen.getByTestId("favorites-search"), { target: { value: "" } });
    fireEvent.change(screen.getByTestId("favorites-kind"), { target: { value: "image" } });
    expect(screen.getByTestId("favorite-f2")).toBeInTheDocument();
    expect(screen.queryByTestId("favorite-f1")).not.toBeInTheDocument();
  });

  it("编辑收藏：保存后列表更新", async () => {
    setTransport(
      (await import("../../test/fixture")).fixtureTransport({
        "GET /api/favorites": () => favorites,
        "GET /api/favorites/:id": () => favorites[0],
        "DELETE /api/favorites/:id": () => null,
        "POST /api/favorites": (_m, _p, body) => ({
          ...favorites[0],
          id: "f1-new",
          text: (body as Record<string, unknown>).text as string,
        }),
      }),
    );
    render(<FavoritesPage ports={new ContentApi()} />);
    await waitFor(() => expect(screen.getByTestId("favorite-f1")).toBeInTheDocument());
    fireEvent.click(screen.getAllByTestId("dot-menu-trigger")[0] as Element);
    fireEvent.click(screen.getByRole("menuitem", { name: "编辑" }));
    fireEvent.change(screen.getByTestId("favorite-edit-text"), { target: { value: "会议纪要（修订）" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(screen.getByText("会议纪要（修订）")).toBeInTheDocument());
  });

  it("复制走 PlatformPort", async () => {
    let copied = "";
    const port: PlatformPort = {
      copyText: async (text) => {
        copied = text;
        return true;
      },
      openExternal: () => undefined,
      saveBlob: () => undefined,
      media: async () => new MediaStream(),
    };
    setPlatform(port);
    setTransport(
      (await import("../../test/fixture")).fixtureTransport({
        "GET /api/favorites": () => favorites,
      }),
    );
    render(<FavoritesPage ports={new ContentApi()} />);
    await waitFor(() => expect(screen.getByTestId("favorite-f1")).toBeInTheDocument());
    fireEvent.click(screen.getAllByTestId("dot-menu-trigger")[0] as Element);
    fireEvent.click(screen.getByRole("menuitem", { name: "复制" }));
    await waitFor(() => expect(copied).toBe("会议纪要"));
  });

  it("删除收藏失败显示错误", async () => {
    setTransport(
      (await import("../../test/fixture")).fixtureTransport({
        "GET /api/favorites": () => favorites,
        "DELETE /api/favorites/:id": () => err(403, "not your favorite"),
      }),
    );
    render(<FavoritesPage ports={new ContentApi()} />);
    await waitFor(() => expect(screen.getByTestId("favorite-f1")).toBeInTheDocument());
    fireEvent.click(screen.getAllByTestId("dot-menu-trigger")[0] as Element);
    fireEvent.click(screen.getByRole("menuitem", { name: "删除" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("没有权限"));
  });
});

describe("ArticlesPage", () => {
  it("上游未开通：显示契约界面提示而不是假数据", async () => {
    setTransport(
      (await import("../../test/fixture")).fixtureTransport({
        "GET /api/articles": () => err(404, "not found"),
        "GET /api/publishers": () => err(404, "not found"),
      }),
    );
    render(<ArticlesPage ports={new ContentApi()} />);
    await waitFor(() =>
      expect(screen.getByText(/文章与订阅号服务（G05 ContentPort）尚未开通/)).toBeInTheDocument(),
    );
  });

  it("成功：渲染文章与关注状态切换", async () => {
    setTransport(
      (await import("../../test/fixture")).fixtureTransport({
        "GET /api/articles": () => [
          {
            article_id: "a1",
            publisher_id: "p1",
            publisher_name: "团队周刊",
            title: "发布 v2 计划",
            doc_html: "<p>正文内容</p>",
            doc_version: 1,
            published_at: now,
          },
        ],
        "GET /api/publishers": () => [
          { publisher_id: "p1", name: "团队周刊", verified: true, subscribed: false, watching: false },
        ],
        "POST /api/publishers/:id/subscription": () => ({
          publisher_id: "p1",
          name: "团队周刊",
          verified: true,
          subscribed: true,
          watching: false,
        }),
        "POST /api/articles/:id/watching": () => null,
      }),
    );
    render(<ArticlesPage ports={new ContentApi()} />);
    await waitFor(() => expect(screen.getByTestId("article-a1")).toBeInTheDocument());
    fireEvent.click(screen.getByTestId("publisher-p1"));
    await waitFor(() => expect(screen.getByTestId("publisher-p1")).toHaveTextContent("已关注"));
    fireEvent.click(screen.getByText("阅读"));
    expect(screen.getByTestId("article-reader")).toHaveTextContent("正文内容");
    fireEvent.click(screen.getByRole("button", { name: "在看" }));
  });
});

describe("StickerPanel", () => {
  it("渲染表情包并记录最近使用", async () => {
    setTransport(
      (await import("../../test/fixture")).fixtureTransport({
        "GET /api/stickers/packs": () => [
          { pack_id: "p1", name: "默认", items: ["😀", "🎉", "👍"] },
        ],
      }),
    );
    const picked: string[] = [];
    render(<StickerPanel ports={new ContentApi()} onPick={(sticker) => picked.push(sticker)} />);
    await waitFor(() => expect(screen.getByTestId("sticker-grid")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "🎉" }));
    expect(picked).toEqual(["🎉"]);
    expect(screen.getByText("最近使用")).toBeInTheDocument();
    expect(localStorage.getItem("wxlike-draft:sticker-recent")).toContain("🎉");
  });
});
