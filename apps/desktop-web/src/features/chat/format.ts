const DAY = 24 * 60 * 60 * 1000;

export function formatRelative(iso: string): string {
  const stamp = new Date(iso).getTime();
  if (!Number.isFinite(stamp)) return "";
  const now = Date.now();
  const date = new Date(stamp);
  const today = new Date(now);
  const sameDay = date.toDateString() === today.toDateString();
  if (sameDay) {
    return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
  }
  const yesterday = new Date(now - DAY);
  if (date.toDateString() === yesterday.toDateString()) return "昨天";
  if (now - stamp < 7 * DAY) return `${date.getMonth() + 1}/${date.getDate()}`;
  return `${date.getMonth() + 1}月${date.getDate()}日`;
}

export function formatClock(iso: string): string {
  const date = new Date(iso);
  if (!Number.isFinite(date.getTime())) return "";
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}
