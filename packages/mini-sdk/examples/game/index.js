// 可玩小游戏（storage）：每局根据上一局分数确定性地猜一个数字。
//
// 游戏不需要网络、相机或位置；清单里也只声明 storage，用来证明示例不会
// 顺手多要权限。存储被拒绝时降级为内存一局，不阻塞启动。
(function (root) {
  "use strict";

  var SCORE = "game.score.v1";
  var TITLE = "猜数字";

  function round(best) {
    var secret = 7;
    var guess = (best % 6) + 1;
    var won = guess === secret;
    return { guess: guess, won: won, best: won ? (best + 1) % 6 : best };
  }

  function runEmbedded(host) {
    try {
      var best = Number(host.storeGet(SCORE) || "0");
      var result = round(best);
      host.storeSet(SCORE, String(result.best));
      host.log("game: guess=" + result.guess + " won=" + result.won);
      host.setTitle(TITLE);
    } catch (err) {
      host.log("game: storage denied (" + err + "), playing in memory");
    }
  }

  async function boot(client) {
    var best = 0;
    var state = { played: false, error: null };
    try {
      best = Number((await client.storage.get(SCORE)) || "0");
      state.result = round(best);
      await client.storage.set(SCORE, String(state.result.best));
      state.played = true;
    } catch (err) {
      state.result = round(0);
      state.error = (err && err.code) || "host";
    }
    return state;
  }

  root.onLoad = function () {
    if (root.wxhost && typeof root.wxhost.storeGet === "function") runEmbedded(root.wxhost);
    if (root.__wxMini) root.__gameReady = boot(root.__wxMini);
  };
  root.onShow = function () {
    if (root.wxhost) root.wxhost.log("game: shown");
  };
  root.__example = { appId: "com.wxlike.demo.game", key: SCORE, title: TITLE, boot: boot };
})(globalThis);
