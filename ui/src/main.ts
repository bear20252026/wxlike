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

  // transient toasts from anywhere in the UI
  let hideTimer = 0;
  appStore.sub(toastAtom, () => {
    const msg = appStore.get(toastAtom);
    let el = document.getElementById("toast");
    if (!msg) {
      el?.remove();
      return;
    }
    if (!el) {
      el = document.createElement("div");
      el.id = "toast";
      document.body.append(el);
    }
    el.textContent = msg;
    el.classList.add("show");
    window.clearTimeout(hideTimer);
    hideTimer = window.setTimeout(() => el?.classList.remove("show"), 2400);
  });
}

main();
