// 笔记/待办示例（storage）。
//
// 真实外壳把 SDK 客户端注入到 globalThis.__wxMini；嵌入式 Boa 宿主只提供
// 同步的 wxhost，因此这里保留一条等价的本地路径，保证同一个包在四端都能
// 启动并保存数据。业务写入只经过 bridge，不碰宿主存储之外的任何东西。
(function (root) {
  "use strict";

  var KEY = "notes.v1";
  var TITLE = "笔记与待办";

  function seed() {
    return [{ id: "t1", text: "给明天的自己留一句话", done: false }];
  }

  function readItems(raw) {
    var items = raw ? JSON.parse(raw) : [];
    return items.length ? items : seed();
  }

  function runEmbedded(host) {
    try {
      var items = readItems(host.storeGet(KEY));
      host.storeSet(KEY, JSON.stringify(items));
      host.log("notes: " + items.length + " item(s)");
      host.setTitle(TITLE);
    } catch (err) {
      host.log("notes: storage denied (" + err + ")");
    }
  }

  async function boot(client) {
    var state = { items: seed(), saved: false, error: null };
    try {
      state.items = readItems(await client.storage.get(KEY));
      await client.storage.set(KEY, JSON.stringify(state.items));
      state.saved = true;
    } catch (err) {
      state.error = (err && err.code) || "host";
    }
    return state;
  }

  root.onLoad = function () {
    if (root.wxhost && typeof root.wxhost.storeGet === "function") runEmbedded(root.wxhost);
    if (root.__wxMini) root.__notesReady = boot(root.__wxMini);
  };
  root.onShow = function () {
    if (root.wxhost) root.wxhost.log("notes: shown");
  };
  root.__example = { appId: "com.wxlike.demo.notes", key: KEY, title: TITLE, boot: boot };
})(globalThis);
