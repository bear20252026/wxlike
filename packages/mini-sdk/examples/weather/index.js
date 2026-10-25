// 天气示例（storage + location）。
//
// 位置是敏感权限：宿主 broker 每次都要确认，返回的坐标已经粗化到两位小数。
// 用户拒绝或取消时退回手动城市，不把经纬度写进日志。
(function (root) {
  "use strict";

  var CITY = "weather.city.v1";
  var TITLE = "天气";

  function coarse(value) {
    return Number(Number(value).toFixed(2));
  }

  function runEmbedded(host) {
    try {
      var city = host.storeGet(CITY) || "杭州";
      host.storeSet(CITY, city);
      host.log("weather: city=" + city);
      host.setTitle(TITLE);
    } catch (err) {
      host.log("weather: storage denied (" + err + ")");
    }
  }

  async function boot(client) {
    var state = { city: null, coords: null, fallback: null, error: null };
    try {
      state.city = (await client.storage.get(CITY)) || "杭州";
      await client.storage.set(CITY, state.city);
      var location = await client.location.get();
      state.coords = { latitude: coarse(location.latitude), longitude: coarse(location.longitude) };
    } catch (err) {
      state.error = (err && err.code) || "host";
      state.fallback = "manual-city";
    }
    return state;
  }

  root.onLoad = function () {
    if (root.wxhost && typeof root.wxhost.storeGet === "function") runEmbedded(root.wxhost);
    if (root.__wxMini) root.__weatherReady = boot(root.__wxMini);
  };
  root.onShow = function () {
    if (root.wxhost) root.wxhost.log("weather: shown");
  };
  root.__example = { appId: "com.wxlike.demo.weather", key: CITY, title: TITLE, boot: boot };
})(globalThis);
