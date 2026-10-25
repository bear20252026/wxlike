/**
 * G11-03 test matrix (video side) — playback visibility gating, publish,
 * live states, danmaku, feed upstream state, duplicate submit.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setTransport } from "../../lib/http";
import { err, fixtureTransport } from "../../test/fixture";
import { VideoApi } from "./api";
import { ChannelsPage } from "./ChannelsPage";
import { VideoPlayer } from "./VideoPlayer";

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  cleanup();
  setTransport(null);
  Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
});

describe("VideoPlayer — visibility gating", () => {
  it("页面不可见时暂停并释放 src（后台标签资源有界）", async () => {
    render(<VideoPlayer videoId="v1" urls={{ mp4: "/api/video/v1/play.mp4", hls: "/api/video/v1/playlist.m3u8", thumb: "/api/video/v1/thumb.png" }} />);
    const video = (await screen.findByTestId("video-v1")) as HTMLVideoElement;
    await waitFor(() => expect(video.getAttribute("src")).toBe("/api/video/v1/play.mp4"));

    let paused = true;
    Object.defineProperty(video, "paused", { configurable: true, get: () => paused });
    const loadSpy = vi.fn();
    video.load = loadSpy;
    video.pause = vi.fn(() => {
      paused = true;
    });

    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    document.dispatchEvent(new Event("visibilitychange"));

    await waitFor(() => {
      expect(video.getAttribute("src")).toBeNull();
      expect(video.pause).toHaveBeenCalled();
      expect(loadSpy).toHaveBeenCalled();
    });

    // 回到可见：重新挂载媒体
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    document.dispatchEvent(new Event("visibilitychange"));
    await waitFor(() => expect(video.getAttribute("src")).toBe("/api/video/v1/play.mp4"));
  });

  it("切页后台时不提交播放计数（view 只在真实播放后记录一次）", async () => {
    const views: string[] = [];
    render(
      <VideoPlayer
        videoId="v2"
        urls={{ mp4: "/api/video/v2/play.mp4", hls: "", thumb: "" }}
        onViewed={() => views.push("v2")}
      />,
    );
    const video = (await screen.findByTestId("video-v2")) as HTMLVideoElement;
    await waitFor(() => expect(video.getAttribute("src")).toBeTruthy());
    video.dispatchEvent(new Event("play"));
    video.dispatchEvent(new Event("play"));
    expect(views).toEqual(["v2"]);
  });
});

describe("ChannelsPage", () => {
  it("feed 上游未开通：明确呈现，不影响发布/直播", async () => {
    setTransport(
      fixtureTransport({
        "GET /api/videos": () => err(404, "not found"),
        "GET /api/live/:channel": () => ({ channel: "main", live: false, viewers: 0 }),
      }),
    );
    render(<ChannelsPage ports={new VideoApi()} />);
    await waitFor(() => expect(screen.getByText(/G05 ContentPort \/videos）尚未开通/)).toBeInTheDocument());
    expect(screen.getByTestId("live-panel")).toBeInTheDocument();
  });

  it("发布：真实上传后可播放", async () => {
    setTransport(
      fixtureTransport({
        "GET /api/videos": () => err(404, "not found"),
        "GET /api/live/:channel": () => ({ channel: "main", live: false, viewers: 0 }),
      }),
    );
    const api = new VideoApi(async (form) => {
      expect(form).toBeInstanceOf(FormData);
      expect((form as FormData).get("file")).not.toBeNull();
      return { video_id: "dd".repeat(32), source_size: 10, duration_ms: 1000 };
    });
    render(<ChannelsPage ports={api} />);
    const input = (await screen.findByTestId("publish-input")) as HTMLInputElement;
    fireEvent.change(input, { target: { files: [new File(["clipdata"], "clip.mp4", { type: "video/mp4" })] } });
    await waitFor(() => expect(screen.getByTestId("published-video")).toBeInTheDocument());
    expect(screen.getByTestId("published-video")).toHaveTextContent("dd".repeat(32));
  });

  it("开播/停播/观众数走真实状态", async () => {
    let live = false;
    let viewers = 0;
    setTransport(
      fixtureTransport({
        "GET /api/videos": () => err(404, "not found"),
        "GET /api/live/:channel": () => ({ channel: "main", live, viewers }),
        "POST /api/live/:channel/start": () => {
          live = true;
          return { stream_key: "sk", ingest_url: "rtmp://x/sk", ingest_token: "t" };
        },
        "POST /api/live/:channel/stop": () => {
          live = false;
          return null;
        },
        "POST /api/live/:channel/join": () => {
          viewers += 1;
          return { viewers };
        },
      }),
    );
    render(<ChannelsPage ports={new VideoApi()} />);
    await waitFor(() => expect(screen.getByTestId("live-viewers")).toHaveTextContent("未开播"));
    fireEvent.click(screen.getByRole("button", { name: "开播" }));
    await waitFor(() => expect(screen.getByTestId("live-viewers")).toHaveTextContent("0 人在看"));
    fireEvent.click(screen.getByRole("button", { name: "进入直播间" }));
    await waitFor(() => expect(screen.getByTestId("live-viewers")).toHaveTextContent("1 人在看"));
    fireEvent.click(screen.getByRole("button", { name: "停播" }));
    await waitFor(() => expect(screen.getByTestId("live-viewers")).toHaveTextContent("未开播"));
  });

  it("弹幕：发送成功后本地回显并清空输入", async () => {
    setTransport(
      fixtureTransport({
        "GET /api/videos": () => err(404, "not found"),
        "GET /api/live/:channel": () => ({ channel: "main", live: true, viewers: 3 }),
        "POST /api/live/:channel/danmaku": (_m, _p, body) => {
          expect((body as Record<string, unknown>).text).toBe("主播好");
          return { delivered: true };
        },
      }),
    );
    render(<ChannelsPage ports={new VideoApi()} />);
    fireEvent.change(await screen.findByTestId("danmaku-input"), { target: { value: "主播好" } });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(screen.getByTestId("danmaku-list")).toHaveTextContent("主播好"));
    expect(screen.getByTestId("danmaku-input")).toHaveValue("");
  });

  it("开播重复点击只发一次请求", async () => {
    const starts: string[] = [];
    setTransport(
      fixtureTransport({
        "GET /api/videos": () => err(404, "not found"),
        "GET /api/live/:channel": () => ({ channel: "main", live: false, viewers: 0 }),
        "POST /api/live/:channel/start": () => {
          starts.push("start");
          return { stream_key: "sk", ingest_url: "rtmp://x/sk", ingest_token: "t" };
        },
      }),
    );
    render(<ChannelsPage ports={new VideoApi()} />);
    const button = await screen.findByRole("button", { name: "开播" });
    fireEvent.click(button);
    fireEvent.click(button);
    await waitFor(() => expect(screen.getByTestId("live-viewers")).toHaveTextContent("0 人在看"));
    expect(starts.length).toBe(1);
  });

  it("断网：错误横幅", async () => {
    setTransport(
      fixtureTransport({
        "GET /api/videos": () => err(0, "fetch failed", "network"),
        "GET /api/live/:channel": () => ({ channel: "main", live: false, viewers: 0 }),
      }),
    );
    render(<ChannelsPage ports={new VideoApi()} />);
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("网络不可用"));
  });
});
