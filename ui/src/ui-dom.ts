// G00-02 纯移动拆分：DOM 构造小工具（自 ui.ts 原样搬出，签名零改动）。
// El 类型随迁；ui.ts 与后续功能模块共用。

export type El = HTMLElement;

export function h(tag: string, cls?: string, text?: string): El {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text !== undefined) el.textContent = text;
  return el;
}

export function clear(el: El): void {
  while (el.firstChild) el.removeChild(el.firstChild);
}

export function on<K extends keyof HTMLElementEventMap>(
  el: El,
  kind: K,
  fn: (e: HTMLElementEventMap[K]) => void,
): void {
  el.addEventListener(kind, fn as EventListener);
}
