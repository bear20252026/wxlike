/**
 * wxlike frontend entry: wires the Tauri event pump into the atom store and
 * mounts the root view. No business logic here (it lives in the Rust
 * commands) — only composition.
 */

import { appStore, toastAtom } from "./state";
import { notifyLost, onInbound, onOutbox } from "./api";
import { renderRoot } from "./ui";
import { invoke, listen } from "./tauri";

function main(): void {
  const root = document.getElementById("app");
  if (!root) throw new Error("#app missing");
  renderRoot(root as HTMLElement);

  // P2 local message library: open before any traffic so the receive pump
  // persists from the first envelope (fire-and-forget: a closed local db
  // must never block the UI — history still works from the server)
  void invoke("local_autopen").catch(() => undefined);

  // hub events → store
  void listen("im-in", (payload) => onInbound(payload));
  void listen("im-down", () => notifyLost());
  // 发送队列的补发/失败事件
  void listen("im-outbox", (payload) => onOutbox(payload));

  // transient toasts from anywhere in the UI.
  // 生命周期只有一个所有者：state.ts 的 toast()（2600 ms 后把 atom 清回
  // null）。这里只跟随 atom：有消息→显示，atom 清空→摘掉 show 类让 CSS
  // 淡出（0.2 s）后移除节点。以前这里另有一个 2400 ms 定时器，两个时钟
  // 各走各的，快速连发 toast 时会出现"字还在、样式已撤"的错拍。
  let removeTimer = 0;
  appStore.sub(toastAtom, () => {
    const msg = appStore.get(toastAtom);
    let el = document.getElementById("toast");
    if (!msg) {
      window.clearTimeout(removeTimer);
      el?.classList.remove("show");
      const node = el;
      removeTimer = window.setTimeout(() => node?.remove(), 250);
      return;
    }
    window.clearTimeout(removeTimer);
    if (!el) {
      el = document.createElement("div");
      el.id = "toast";
      document.body.append(el);
    }
    el.textContent = msg;
    el.classList.add("show");
  });
}

main();
