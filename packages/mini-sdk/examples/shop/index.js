// 示例商店（storage + network + payment）。
//
// 网络只允许 GET https://api.wxlike.app/...；支付走宿主 broker，返回的是
// 收据（reference + status），不是令牌。开发渠道的支付状态是 sandbox：
// 演示流程完整，但从不代表真实扣款。用户拒绝支付时保留购物车并标记取消。
(function (root) {
  "use strict";

  var CART = "shop.cart.v1";
  var MENU_URL = "https://api.wxlike.app/mini/shop/menu";
  var ORDER_ID = "demo-order-0001";
  var AMOUNT = "18.00";
  var CURRENCY = "CNY";
  var TITLE = "示例商店";

  function defaultCart() {
    return [{ sku: "coffee", qty: 1, price: "18.00" }];
  }

  function readCart(raw) {
    var lines = raw ? JSON.parse(raw) : [];
    return lines.length ? lines : defaultCart();
  }

  function runEmbedded(host) {
    try {
      var cart = readCart(host.storeGet(CART));
      host.storeSet(CART, JSON.stringify(cart));
      host.log("shop: " + cart.length + " line(s), checkout requires network + payment");
      host.setTitle(TITLE);
    } catch (err) {
      host.log("shop: storage denied (" + err + ")");
    }
  }

  async function boot(client) {
    var state = { cart: defaultCart(), menu: null, receipt: null, sandbox: true, error: null };
    try {
      state.cart = readCart(await client.storage.get(CART));
      await client.storage.set(CART, JSON.stringify(state.cart));
      state.menu = await client.network.request(MENU_URL);
      state.receipt = await client.payments.pay({
        amount: AMOUNT,
        currency: CURRENCY,
        orderId: ORDER_ID,
      });
    } catch (err) {
      state.error = (err && err.code) || "host";
    }
    return state;
  }

  root.onLoad = function () {
    if (root.wxhost && typeof root.wxhost.storeGet === "function") runEmbedded(root.wxhost);
    if (root.__wxMini) root.__shopReady = boot(root.__wxMini);
  };
  root.onShow = function () {
    if (root.wxhost) root.wxhost.log("shop: shown, sandbox checkout only");
  };
  root.__example = { appId: "com.wxlike.demo.shop", key: CART, orderId: ORDER_ID, boot: boot };
})(globalThis);
