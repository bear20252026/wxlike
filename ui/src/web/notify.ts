/**
 * System notifications (A4).
 *
 * `Notification.requestPermission()` only produces a prompt from a user
 * gesture, so it is called from the login command — that click is the gesture.
 * A refusal is remembered by the browser and simply means "no notifications";
 * nothing else in the app changes.
 */

export async function requestNotifyPermission(): Promise<void> {
  if (!("Notification" in globalThis)) return;
  if (Notification.permission === "default") {
    try {
      await Notification.requestPermission();
    } catch {
      /* some browsers require a transient user activation; login click gives it */
    }
  }
}

export function showNotification(title: string, body: string): void {
  if (!("Notification" in globalThis) || Notification.permission !== "granted") return;
  try {
    const n = new Notification(title, { body, tag: title });
    n.onclick = () => {
      window.focus();
      n.close();
    };
  } catch {
    // Chrome throws for non-persisted notifications without a service worker
    // registration: a missing banner must not break message delivery.
  }
}
