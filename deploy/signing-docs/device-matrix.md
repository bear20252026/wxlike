# 真机设备矩阵

`device-matrix.json` 是**要求覆盖**，不是结果。没有真机跑过时这里没有证据，
发布门禁必须失败。

| 设备 | 系统 | 覆盖重点 |
|---|---|---|
| Pixel 7 | Android 14 | 默认权限流程、FCM 到达、WebView 最小程序 |
| Galaxy A54 | Android 13 | 厂商 ROM 后台限制、厂商推送通道、重连 |
| iPhone 13 | iOS 17 | APNs、后台音频、WebView |
| iPhone SE (2nd) | iOS 16 | 小屏布局、旧系统权限文案、低内存 |

每台设备每个 feature 都要有一条记录：

| feature | 怎么算通过 |
|---|---|
| `push` | 杀死进程后收到通知，点开进入正确会话；日志有推送消息 id |
| `call` | 真实 TURN/SFU 下双向音视频 ≥60 秒，弱网重连后仍可通话 |
| `webview` | 小程序打开、授权、返回；进程被杀后重进状态正确 |
| `permissions` | 首次拒绝/允许/再次进入三条路径都有截图或录屏 |
| `reconnect` | 飞行模式 30 秒恢复后，消息补齐且无重复 |

## 提交方式

1. 用 `device-evidence.schema.json` 生成 `mobile-evidence/device-evidence.json`。
2. 证据文件（截图/录屏/日志）与 JSON 同目录，`evidence` 写相对路径。
3. `git_sha` 必须是产生 APK/IPA 的提交；`artifact_sha256` 必须与
   `android-release.json`/`ios-release.json` 一致。
4. 跑：

```bash
python ci/jobs/mobile/check_device_evidence.py \
  --evidence mobile-evidence/device-evidence.json \
  --matrix deploy/signing-docs/device-matrix.json \
  --release-evidence mobile-evidence/android-release.json
```

校验器会拒绝：缺失设备、缺 feature、`status=fail`、占位文本
（`TBD`/`example`/`CHANGEME`）、证据文件不存在或为空、artifact hash 不一致。

## 当前状态

**没有**任何真机记录。`MOBILE-DEVICE-EVIDENCE` 缺口保持打开；`release` job
不得在没有通过上述校验的情况下发布移动端。
