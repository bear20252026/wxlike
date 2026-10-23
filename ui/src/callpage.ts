/**
 * 通话页 + 设备验证页（P4/P5）。
 *
 * 通话页的布局与控件集参照 Nextcloud Talk 的 `CallView`（参与者网格 + 底部
 * 控制条 + 参与者操作菜单），设备验证页参照 Element Web 的
 * `DeviceVerification` 文案与"12 组 5 位数字"呈现。
 *
 * 两个页面都只经 `api.ts` 走 Tauri 命令，不直接开 HTTP（任务书 §0.1.1）。
 */

import * as api from "./api";
import { appStore, meAtom, toast, viewAtom } from "./state";
import { listen } from "./tauri";

type El = HTMLElement;

function h(tag: string, cls?: string, text?: string): El {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text !== undefined) el.textContent = text;
  return el;
}

function clear(el: El): void {
  while (el.firstChild) el.removeChild(el.firstChild);
}

/** 轮询句柄：离开页面时必须停掉，否则会对已卸载的 DOM 继续刷新。 */
let poll: number | null = null;

/** 本地媒体引擎的最近一次快照（`call://rtc` 推送）。 */
let rtc: api.CallRtcStatus | null = null;
let rtcUnsub: (() => void) | null = null;

export function stopCallPolling(): void {
  if (poll !== null) {
    window.clearInterval(poll);
    poll = null;
  }
  if (rtcUnsub) {
    rtcUnsub();
    rtcUnsub = null;
  }
  rtc = null;
  // 离开通话页就挂断媒体：线程不挂会一直占着 UDP 端口和麦克风
  void api.callRtcHangup().catch(() => {});
}

function mmss(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

function shortId(id: string): string {
  return id.length > 8 ? `${id.slice(0, 6)}…` : id;
}

/** 参与者操作菜单（参照 spreed 的 ParticipantItem 菜单）。 */
function participantMenu(anchor: El, convId: string, user: string, isHost: boolean): void {
  document.querySelectorAll(".call-menu").forEach((n) => n.remove());
  const menu = h("div", "call-menu");
  const item = (label: string, run: () => void): void => {
    const b = h("button", "call-menu-item", label);
    b.addEventListener("click", () => {
      menu.remove();
      run();
    });
    menu.append(b);
  };
  item("静音其麦克风", () => void act(convId, "mute", { target: user, track: "audio", value: true }));
  item("关闭其摄像头", () => void act(convId, "mute", { target: user, track: "video", value: true }));
  if (isHost) {
    item("移交主持", () => void act(convId, "transfer", { target: user }));
    item("移出通话", () => void act(convId, "kick", { target: user }));
  }
  const r = anchor.getBoundingClientRect();
  menu.style.left = `${Math.round(r.left)}px`;
  menu.style.top = `${Math.round(r.bottom + 6)}px`;
  document.body.append(menu);
  const close = (ev: MouseEvent): void => {
    if (!menu.contains(ev.target as Node)) {
      menu.remove();
      document.removeEventListener("click", close, true);
    }
  };
  window.setTimeout(() => document.addEventListener("click", close, true), 0);
}

async function act(
  convId: string,
  action: api.CallAction,
  opts: Parameters<typeof api.callAction>[2] = {},
): Promise<void> {
  try {
    await api.callAction(convId, action, opts);
    if (action === "leave") {
      const v = appStore.get(viewAtom);
      if (v.screen === "call") {
        appStore.set(viewAtom, { screen: "chat", convId: v.convId, title: v.title, isGroup: v.isGroup });
      }
      return;
    }
    redraw?.();
  } catch (e) {
    toast(String(e).slice(0, 120));
  }
}

let redraw: (() => void) | null = null;

/** 通话页：进页面自动 start/join，然后每 4 s 拉一次快照。 */
export function renderCallPage(root: El, convId: string, title: string, isGroup: boolean): void {
  stopCallPolling();
  clear(root);
  const me = appStore.get(meAtom);

  const head = h("div", "call-head");
  const back = h("button", "icon-btn", "←");
  back.title = "返回聊天";
  back.addEventListener("click", () => {
    appStore.set(viewAtom, { screen: "chat", convId, title, isGroup });
  });
  const headText = h("div", "call-head-text");
  const nameEl = h("div", "call-title", title);
  const metaEl = h("div", "call-meta", "连接中…");
  headText.append(nameEl, metaEl);
  head.append(back, headText);
  const body = h("div", "call-body");
  const bar = h("div", "call-bar");
  root.append(head, body, bar);

  let latest: api.CallState | null = null;
  let startedAt = Date.now();

  const draw = (): void => {
    clear(body);
    clear(bar);
    const st = latest;
    if (!st || !st.live) {
      body.append(h("div", "call-empty", "通话已结束"));
      const again = h("button", "btn", "重新发起");
      again.addEventListener("click", () => void bootstrap());
      body.append(again);
      return;
    }
    const host = st.host ?? "";
    const isHost = host === me?.user_id;
    const mine = st.participants.find((p) => p.user === me?.user_id);
    metaEl.textContent = `${st.participants.length} 人在通话 · ${mmss(Date.now() - startedAt)}${
      st.locked ? " · 已锁门" : ""
    }${st.recording ? " · 录制中" : ""}`;

    const grid = h("div", "call-grid");
    for (const p of st.participants) {
      const tile = h("div", "call-tile");
      const avatar = h("div", "call-avatar", (p.user[0] ?? "?").toUpperCase());
      if (p.hand_raised) avatar.classList.add("hand");
      const label = h("div", "call-name", p.user === me?.user_id ? "我" : shortId(p.user));
      if (p.user === host) label.append(h("span", "call-badge", "主持"));
      const flags = h("div", "call-flags");
      if (p.audio_muted) flags.append(h("span", "flag", "🔇"));
      if (p.video_muted) flags.append(h("span", "flag", "📷"));
      if (p.hand_raised) flags.append(h("span", "flag", "✋"));
      tile.append(avatar, label, flags);
      if (p.user !== me?.user_id) {
        tile.title = "点击管理该参与者";
        tile.addEventListener("click", () => participantMenu(tile, convId, p.user, isHost));
      }
      grid.append(tile);
    }
    body.append(grid);

    // -- 底部控制条
    const selfMuted = mine?.audio_muted ?? false;
    const btn = (label: string, cls: string, on: boolean, run: () => void): El => {
      const b = h("button", `call-btn ${cls}`, label);
      if (on) b.classList.add("on");
      b.addEventListener("click", run);
      return b;
    };
    bar.append(
      btn(selfMuted ? "🎤 已静音" : "🎤 麦克风", "mic", selfMuted, () =>
        void act(convId, "mute", { target: me?.user_id, track: "audio", value: !selfMuted }),
      ),
      btn("✋ 举手", "hand", mine?.hand_raised ?? false, () =>
        void act(convId, "hand", { value: !(mine?.hand_raised ?? false) }),
      ),
    );
    if (isHost) {
      bar.append(
        btn("🔒 锁门", "lock", st.locked ?? false, () => void act(convId, "lock", { value: !st.locked })),
        btn("⏺ 录制", "rec", st.recording ?? false, () => void act(convId, "record", { value: !st.recording })),
        btn("🔇 全员静音", "muteall", false, () =>
          void act(convId, "mute-all", { track: "audio", value: true }),
        ),
      );
    }
    // 语音直连：对房里除自己以外的每个人各开一条媒体会话（mesh）。
    // 人数多了要 SFU，这里给出上限提示而不是假装能用。
    const others = st.participants
      .filter((p) => p.user !== me?.user_id)
      .map((p) => p.user);
    if (me && others.length > 0) {
      const live = rtc?.active && rtc.conv === convId;
      if (live && rtc && rtc.connected_peers > 0) {
        const total = rtc.peers;
        bar.append(
          h(
            "span",
            "call-btn rtc on",
            `🎧 ${rtc.connected_peers}/${total} · ↓${rtc.audio_frames} ↑${rtc.mic_frames}`,
          ),
        );
      } else if (live && rtc && rtc.ringing) {
        bar.append(btn("📞 接听", "rtc", false, () => void api.callRtcAccept()));
      } else {
        const on = live === true;
        bar.append(
          btn(on ? "🎧 拨号中…" : `🎧 语音直连（${others.length}）`, "rtc", on, () => {
            // 音频 mesh 的上行 ≈ 50 kbps × 人数（Nextcloud Talk 的算法）；
            // 8 人约 350 kbps，再多人就该上 SFU 了。
            if (others.length > 8) {
              toast("音频 mesh 建议 ≤ 8 人；更多人需要 SFU（尚未实现）");
            }
            void (async () => {
              try {
                await api.callRtcStart(convId, others, false);
                await refresh();
              } catch (e) {
                toast(String(e).slice(0, 120));
              }
            })();
          }),
        );
      }
    }
    const leave = h("button", "call-btn hangup", "📞 挂断");
    leave.addEventListener("click", () => {
      void api.callRtcHangup().catch(() => {});
      void act(convId, "leave");
    });
    bar.append(leave);
  };
  redraw = draw;

  const refresh = async (): Promise<void> => {
    try {
      latest = await api.callState(convId);
      // 计时从最早进房的人算起（房主先开房，后来者不能把计时清零）
      const first = latest.participants.reduce(
        (min, p) => (min === 0 ? p.joined_at : Math.min(min, p.joined_at)),
        0,
      );
      if (first > 0) startedAt = first;
      draw();
    } catch (e) {
      metaEl.textContent = String(e).slice(0, 80);
    }
  };

  const bootstrap = async (): Promise<void> => {
    try {
      const st = await api.callState(convId);
      if (!st.live) {
        await api.callAction(convId, "start", { capacity: 50 });
      }
      await api.callAction(convId, "join");
    } catch (e) {
      toast(String(e).slice(0, 120));
    }
    await refresh();
  };

  void listen("call://rtc", (payload) => {
    rtc = payload as api.CallRtcStatus;
    draw();
  }).then((un) => {
    rtcUnsub = un;
  });
  void api.callRtcStatus().then((st) => {
    rtc = st;
    draw();
  });

  void bootstrap();
  poll = window.setInterval(() => void refresh(), 4000);
}

/** 设备验证页：安全码 12 组 × 5 位 + 身份指纹 + 标记已验证。 */
export function renderVerifyPage(root: El, peerId: string, title: string): void {
  stopCallPolling();
  clear(root);

  const head = h("div", "call-head");
  const back = h("button", "icon-btn", "←");
  back.title = "返回聊天";
  back.addEventListener("click", () => {
    const v = appStore.get(viewAtom);
    if (v.screen === "verify") {
      appStore.set(viewAtom, { screen: "main", tab: "chats" });
    }
  });
  const headText = h("div", "call-head-text");
  headText.append(h("div", "call-title", `验证 ${title} 的设备`));
  headText.append(h("div", "call-meta", "逐组比对，一致才标记"));
  head.append(back, headText);
  const body = h("div", "verify-body");
  root.append(head, body);

  const draw = async (): Promise<void> => {
    clear(body);
    let info: api.SafetyInfo;
    try {
      info = await api.safetyNumber(peerId);
    } catch (e) {
      body.append(h("div", "call-empty", String(e).slice(0, 140)));
      return;
    }
    const verified = info.verified_at !== null;
    const status = h("div", verified ? "verify-status ok" : "verify-status", verified ? "✅ 已验证" : "⚠️ 未验证");
    const hint = h(
      "p",
      "verify-hint",
      "让对方打开同一页面，从第 1 组开始逐组读出数字。全部一致说明两端密钥未被中间人替换；有任何一组不同就立即停止并重新建立会话。",
    );
    body.append(status, hint);

    const groups = info.safety_number.match(/.{1,5}/g) ?? [info.safety_number];
    const grid = h("div", "verify-grid");
    groups.forEach((g, i) => {
      const cell = h("div", "verify-cell");
      cell.append(h("div", "verify-idx", String(i + 1)));
      cell.append(h("div", "verify-digits", g));
      grid.append(cell);
    });
    body.append(grid);

    const fp = h("div", "verify-fp");
    fp.append(h("div", "verify-fp-label", "对端身份指纹"));
    const code = h("code", "verify-fp-code", info.peer_identity);
    fp.append(code);
    const copy = h("button", "btn ghost", "复制");
    copy.addEventListener("click", () => {
      // 非安全上下文里 `navigator.clipboard` 本身就是 undefined；
      // 失败要给一句话，不能留一个没人接的 rejection。
      const fail = () => toast("复制失败，请手动选中指纹");
      try {
        void navigator.clipboard
          .writeText(info.peer_identity)
          .then(() => toast("指纹已复制"), fail);
      } catch {
        fail();
      }
    });
    fp.append(copy);
    body.append(fp);

    if (!verified) {
      const ok = h("button", "btn", "我已逐组核对，标记为已验证");
      ok.addEventListener("click", () => {
        ok.setAttribute("disabled", "true");
        void api
          .verifyPeer(peerId, info.peer_identity)
          .then(() => {
            toast("已标记验证");
            void draw();
          })
          .catch((e) => {
            ok.removeAttribute("disabled");
            toast(String(e).slice(0, 120));
          });
      });
      body.append(ok);
    }
  };
  void draw();
}
