/**
 * 视频号 (G11-03): 关注/热门/发布/直播面板 with real states.
 * - 发布：真实上传到 POST /api/video（上传后可直接播放）。
 * - 直播：开播/停播/观众数/弹幕走真实 /api/live/*；连麦（RTC）走 calls 域。
 * - 关注/热门 feed：G05 ContentPort /videos 契约，上游未开通时明确呈现。
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, errorText } from "../../lib/http";
import type { DanmakuMessage, FeedTab, VideoFeedItem, VideoMeta, VideoPorts } from "./model";
import { playbackUrls as urlsOf } from "./api";
import { VideoPlayer } from "./VideoPlayer";
import { Banner, EmptyState, GhostButton, PrimaryButton, formatTime } from "../content/ui";

const UPSTREAM_FEED = "视频号 feed（G05 ContentPort /videos）尚未开通";

export function ChannelsPage({ ports }: { ports: VideoPorts }) {
  const [tab, setTab] = useState<FeedTab>("trending");
  const [feed, setFeed] = useState<VideoFeedItem[] | null>(null);
  const [feedMissing, setFeedMissing] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [channel, setChannel] = useState("main");  const [live, setLive] = useState(false);
  const [viewers, setViewers] = useState(0);
  const [danmaku, setDanmaku] = useState<DanmakuMessage[]>([]);
  const [danmakuText, setDanmakuText] = useState("");
  const [publishing, setPublishing] = useState(false);
  const [published, setPublished] = useState<VideoMeta | null>(null);
  const publishAbort = useRef<AbortController | null>(null);
  const publishInFlight = useRef(false);
  const liveInFlight = useRef(false);

  useEffect(() => {
    let alive = true;
    void ports
      .feed(tab)
      .then((items) => {
        if (!alive) return;
        setFeed(items);
        setFeedMissing(false);
      })
      .catch((err: unknown) => {
        if (!alive) return;
        if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
          setFeedMissing(true);
          setFeed([]);
        } else {
          setError(errorText(err));
        }
      });
    return () => {
      alive = false;
    };
  }, [ports, tab]);

  const refreshLive = useCallback(async () => {
    try {
      const state = await ports.liveState(channel);
      setLive(state.live);
      setViewers(state.viewers ?? 0);
    } catch (err) {
      if (!(err instanceof ApiError && err.status === 404)) setError(errorText(err));
    }
  }, [ports, channel]);

  useEffect(() => {
    void refreshLive();
  }, [refreshLive]);

  const toggleLive = async () => {
    if (liveInFlight.current) return; // 写操作重复提交
    liveInFlight.current = true;
    setError("");
    try {
      if (live) {
        await ports.liveStop(channel);
        setLive(false);
        setNotice("直播已结束");
      } else {
        const reply = await ports.liveStart(channel, false);
        setLive(true);
        setNotice(`已开播：推流地址 ${reply.ingest_url}`);
      }
    } catch (err) {
      setError(errorText(err));
    } finally {
      liveInFlight.current = false;
    }
  };

  const joinLive = async () => {
    setError("");
    try {
      const reply = await ports.liveJoin(channel);
      setViewers(reply.viewers);
    } catch (err) {
      setError(errorText(err));
    }
  };

  const sendDanmaku = async () => {
    const text = danmakuText.trim();
    if (!text) return;
    setError("");
    try {
      const message = await ports.sendDanmaku(channel, text);
      setDanmaku((prev) => [...prev.slice(-50), message]);
      setDanmakuText("");
    } catch (err) {
      setError(errorText(err));
    }
  };

  const publish = async (file: File) => {
    if (publishInFlight.current) return;
    publishInFlight.current = true;
    setPublishing(true);
    setError("");
    const controller = new AbortController();
    publishAbort.current = controller;
    try {
      const meta = await ports.uploadVideo(file, { signal: controller.signal });
      setPublished(meta);
      setNotice(`发布成功：${meta.video_id}`);
    } catch (err) {
      setError(errorText(err));
    } finally {
      publishInFlight.current = false;
      setPublishing(false);
      publishAbort.current = null;
    }
  };

  return (
    <div className="mx-auto flex h-full w-full max-w-2xl flex-col gap-3 overflow-y-auto p-4" data-testid="channels-page">
      <div className="flex items-center gap-2">
        {(["following", "trending"] as const).map((item) => (
          <button
            key={item}
            type="button"
            onClick={() => setTab(item)}
            data-testid={`feed-tab-${item}`}
            className={`rounded-lg px-3 py-1.5 text-sm ${tab === item ? "bg-emerald-600 text-white" : "bg-white text-zinc-600 hover:bg-zinc-100"}`}
          >
            {item === "following" ? "关注" : "热门"}
          </button>
        ))}
      </div>

      <Banner tone="error" text={error} />
      <Banner tone="info" text={notice} />
      <Banner tone="info" text={feedMissing ? `${UPSTREAM_FEED}；以下为发布与直播的真实功能。` : ""} />

      {feed !== null && !feedMissing && feed.length === 0 ? (
        <EmptyState title="暂无视频" hint="发布第一个视频吧" />
      ) : null}
      {(feed ?? []).map((item) => (
        <article key={item.video_id} className="rounded-xl border border-zinc-200 bg-white p-3.5" data-testid={`feed-item-${item.video_id}`}>
          <div className="flex items-center justify-between text-xs text-zinc-400">
            <span>
              {item.author_name} · {formatTime(item.published_at)}
            </span>
            <span>{item.followed ? "已关注" : "关注"}</span>
          </div>
          <h3 className="mt-1 text-sm font-semibold text-zinc-800">{item.title}</h3>
          <div className="mt-2">
            <VideoPlayer
              videoId={item.video_id}
              urls={urlsOf(item.video_id)}
              onViewed={() => void ports.recordView(item.video_id)}
            />
          </div>
        </article>
      ))}

      {published ? (
        <section aria-label="刚发布的视频" className="rounded-xl border border-zinc-200 bg-white p-3.5" data-testid="published-video">
          <p className="text-xs text-zinc-400">刚发布 · {published.video_id}</p>
          <div className="mt-2">
            <VideoPlayer videoId={published.video_id} urls={urlsOf(published.video_id)} meta={published} />
          </div>
        </section>
      ) : null}

      <section aria-label="发布视频" className="rounded-xl border border-zinc-200 bg-white p-3.5">
        <label className="inline-flex cursor-pointer items-center gap-2 text-sm">
          <PrimaryButton busy={publishing} onClick={() => undefined} title="发布视频">
            {publishing ? "发布中…" : "选择视频发布"}
          </PrimaryButton>
          {publishing ? (
            <GhostButton onClick={() => publishAbort.current?.abort()}>取消</GhostButton>
          ) : null}
          <input
            type="file"
            accept="video/*"
            className="hidden"
            data-testid="publish-input"
            onChange={(event) => {
              const file = event.target.files?.[0];
              event.target.value = "";
              if (file) void publish(file);
            }}
          />
        </label>
      </section>

      <section aria-label="直播" className="rounded-xl border border-zinc-200 bg-white p-3.5" data-testid="live-panel">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <input
              value={channel}
              onChange={(event) => setChannel(event.target.value.trim() || "main")}
              aria-label="直播间 ID"
              data-testid="live-channel-input"
              className="w-28 rounded-md border border-zinc-200 px-2 py-1 text-sm"
            />
            {live ? <span className="rounded bg-red-600 px-1.5 py-0.5 text-[10px] text-white">LIVE</span> : null}
          </div>
          <div className="flex gap-2">
            <GhostButton onClick={() => void joinLive()} disabled={!live}>
              进入直播间
            </GhostButton>
            <GhostButton tone={live ? "danger" : "neutral"} onClick={() => void toggleLive()}>
              {live ? "停播" : "开播"}
            </GhostButton>
          </div>
        </div>
        <p className="mt-1 text-xs text-zinc-400" data-testid="live-viewers">
          {live ? `${viewers} 人在看` : "未开播"}
        </p>
        <div className="mt-2 flex gap-2">
          <input
            value={danmakuText}
            onChange={(event) => setDanmakuText(event.target.value)}
            placeholder="发个弹幕…"
            maxLength={100}
            data-testid="danmaku-input"
            className="flex-1 rounded-md border border-zinc-200 px-2 py-1.5 text-sm outline-none focus:border-emerald-500"
          />
          <PrimaryButton onClick={() => void sendDanmaku()} disabled={!danmakuText.trim()}>
            发送
          </PrimaryButton>
        </div>
        <ul className="mt-2 max-h-32 space-y-1 overflow-y-auto text-sm" data-testid="danmaku-list">
          {danmaku.map((message) => (
            <li key={message.id} className={message.self ? "text-emerald-700" : "text-zinc-600"}>
              {message.author}：{message.text}
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}
