# 小程序示例与开发工具链

这四个示例是最小生态：它们声明什么能力，就只使用什么能力。每个目录都是一
个可打包的源码目录（`index.js` + `mini.template.json`），四端共用同一个签名包。

| 示例 | app_id | 能力 | 覆盖 |
| --- | --- | --- | --- |
| `notes` | `com.wxlike.demo.notes` | storage | 待办列表读写；存储被拒绝时降级 |
| `shop` | `com.wxlike.demo.shop` | storage, network, payment | 只允许 `api.wxlike.app` 的网络请求；沙箱支付收据；拒绝/取消不丢购物车 |
| `game` | `com.wxlike.demo.game` | storage | 可玩猜数字；最高分跨启动保存 |
| `weather` | `com.wxlike.demo.weather` | storage, location | 粗化坐标；拒绝/取消退回手动城市 |

## 运行方式

示例是**经典脚本**，不依赖打包器。真实外壳把 SDK 客户端注入为
`globalThis.__wxMini`（见 `packages/mini-sdk/src/index.js`），宿主再调用
`onLoad` / `onShow`。嵌入式 Boa 宿主只提供同步的 `wxhost`，示例保留一条等价
本地路径，用于四端同包启动与持久化测试。

`ui-cards.json` 是 UI v2 小程序卡片的审计表：有签名包或已授权真实应用的卡片
才允许渲染和启动；第三方品牌卡在取得发布者授权前一律 `blocked_external`，
不能用示例包冒充。

## 打包、签名与检查

```bash
# 1) 打包（填写资源摘要并嵌入 mini.json）
cargo run -p wxlike-miniapps --bin mini -- pack packages/mini-sdk/examples/notes --out /tmp/notes.dpage

# 2) 签名（种子只从参数/环境读，不回显；默认写成 /tmp/notes.sig）
MINI_SIGNING_KEY=<64 位十六进制种子> \
  cargo run -p wxlike-miniapps --bin mini -- sign /tmp/notes.dpage --key-id pub-1

# 3) 检查（无签名材料时明确显示 unverified）
cargo run -p wxlike-miniapps --bin mini -- inspect /tmp/notes.dpage --sig /tmp/notes.sig --public-key <公钥>

# 4) 本地开发服务（仅 dev 渠道，只提供已封入清单的文件）
cargo run -p wxlike-miniapps --bin mini -- dev-serve packages/mini-sdk/examples/notes --port 8080
```

支付示例使用 `sandbox` 收据：流程完整，但没有任何真实扣款；示例会显式标记
`sandbox: true`，不得把演示收据当作真实支付凭证。
