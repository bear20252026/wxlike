/**
 * wxlike frontend entry: wires the Tauri event pump into the atom store and
 * mounts the root view. No business logic here (it lives in the Rust
 * commands) — only composition.
 */

import { appStore, toastAtom } from "./state";
import { notifyLost, onInbound } from "./api";
import { renderRoot } from "./ui";
import { listen } from "./tauri";

function main(): void {
  const root = document.getElementById("app");
  if (!root) throw new Error("#app missing");
  renderRoot(root as HTMLElement);

  // hub events → store
  void listen("im-in", (payload) => onInbound(payload));
  void listen("im-down", () => notifyLost());

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
