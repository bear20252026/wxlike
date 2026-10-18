/**
 * VENDORED from Telegram Web (tweb) — GPL-3.0.
 *
 *   source : https://github.com/morethanwords/tweb
 *   path   : src/helpers/listenerSetter.ts
 *   why    : 事件监听器的集中登记与一次性清理。我们原来在两个菜单里各写一遍
 *            `document.addEventListener("pointerdown", function away(){ ... })`，
 *            只在"下一次点击"时自清；菜单被别的路径关掉（切视图/重渲染）时
 *            监听器会一直挂着。tweb 的 ListenerSetter 就是解决这个的。
 *
 * Adaptations (minimal):
 *   1. dropped the `EventListenerBase` type import (tweb-internal class) and
 *      removed it from the `ListenerElement` union;
 *   2. `removeAll` 是 tweb 的原始方法名，调用方直接使用。
 * Origin recorded in THIRD-PARTY-NOTICES.md.
 */


export type Listener = {
  element: ListenerElement,
  event: ListenerEvent,
  callback: ListenerCallback,
  options?: ListenerOptions,

  onceFired?: true, // will be set only when options.once is set
  onceCallback?: () => void,
};

export type ListenerElement = Window | Document | HTMLElement | Element | EventTarget;
export type ListenerEvent = string;
export type ListenerCallback = Function;
export type ListenerOptions = AddEventListenerOptions;

/* const originalAddEventListener = HTMLElement.prototype.addEventListener;
HTMLElement.prototype.addEventListener = function(this, name: string, callback: EventListenerOrEventListenerObject, options: AddEventListenerOptions) {
  console.log('nu zdarova', name);
  originalAddEventListener.call(this, name, callback, options);

  if(options?.ls) {
    return options.ls.addFromElement(this, name, callback as any, options);
  }
}; */

export default class ListenerSetter {
  private listeners: Set<Listener> = new Set();
  private cleanups: Set<() => void> = new Set();

  public add<T extends ListenerElement>(element: T): T['addEventListener'] {
    return ((event: string, callback: Function, options: ListenerOptions) => {
      const listener: Listener = {element, event, callback, options};
      this.addManual(listener);
      return listener;
    }) as any;
  }

  /* public addFromElement<T extends ListenerElement>(element: T, event: ListenerEvent, callback: ListenerCallback, options: ListenerOptions) {
    const listener: Listener = {element, event, callback, options};
    this.addManual(listener);
    return listener;
  } */

  public addManual(listener: Listener) {
    // @ts-ignore
    listener.element.addEventListener(listener.event, listener.callback, listener.options);

    if(listener.options?.once) { // remove listener when its called
      listener.onceCallback = () => {
        this.remove(listener);
        listener.onceFired = true;
      };

      // @ts-ignore
      listener.element.addEventListener(listener.event, listener.onceCallback, listener.options);
    }

    this.listeners.add(listener);
  }

  public remove(listener: Listener) {
    if(!listener.onceFired) {
      // @ts-ignore
      listener.element.removeEventListener(listener.event, listener.callback, listener.options);

      if(listener.onceCallback) {
        // @ts-ignore
        listener.element.removeEventListener(listener.event, listener.onceCallback, listener.options);
      }
    }

    this.listeners.delete(listener);
  }

  public removeManual<T extends ListenerElement>(
    element: T,
    event: ListenerEvent,
    callback: ListenerCallback,
    options?: ListenerOptions
  ) {
    // strict-mode adaptation: may stay unset when nothing matches
    let listener: Listener | undefined;
    for(const _listener of this.listeners) {
      if(_listener.element === element &&
        _listener.event === event &&
        _listener.callback === callback &&
        _listener.options === options) {
        listener = _listener;
        break;
      }
    }

    if(listener) {
      this.remove(listener);
    }
  }

  public addCleanup(cleanup: () => void) {
    this.cleanups.add(cleanup);
    return () => this.cleanups.delete(cleanup);
  }

  public removeAll() {
    this.listeners.forEach((listener) => {
      this.remove(listener);
    });
    this.cleanups.forEach((cleanup) => cleanup());
    this.cleanups.clear();
  }
}
