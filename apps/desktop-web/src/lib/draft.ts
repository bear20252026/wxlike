/**
 * Draft persistence (G11-01 requirement 3): 加载与保存错误保留草稿.
 *
 * Composer state lives in localStorage under a per-scope key. Save errors in
 * the feature code call `keepDraft` instead of clearing, so a failed publish
 * or a feed load failure never loses the user's text; a browser refresh picks
 * the draft back up (验收: 笔记刷新保留).
 */

const PREFIX = "wxlike-draft:";

/** Storage failures are swallowed for the user, but visible in dev builds. */
function warnDev(scope: string, action: string, err: unknown): void {
  if (import.meta.env.DEV) {
    console.warn(`[draft] ${action} failed for scope "${scope}":`, err);
  }
}

export function loadDraft<T>(scope: string): T | null {
  try {
    const raw = localStorage.getItem(PREFIX + scope);
    return raw ? (JSON.parse(raw) as T) : null;
  } catch (err) {
    warnDev(scope, "load", err);
    return null;
  }
}

export function saveDraft<T>(scope: string, draft: T): void {
  try {
    localStorage.setItem(PREFIX + scope, JSON.stringify(draft));
  } catch (err) {
    // quota/exotic storage failures must not crash the composer
    warnDev(scope, "save", err);
  }
}

export function clearDraft(scope: string): void {
  try {
    localStorage.removeItem(PREFIX + scope);
  } catch (err) {
    warnDev(scope, "clear", err);
  }
}
