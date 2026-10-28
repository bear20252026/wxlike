# NOTICE 缺口索引（2026-10-24 快照）

> **本清单已作废（2026-10-28）**：许可证这道闸已按 G00 决定删除——`sourceaudit notice`
> 门禁与 `notice-waivers.json` 一并移除，NOTICE 覆盖不再阻断任何检查。下面的表是当时的
> 快照，留着只为了回答"当初缺哪些"，不是待办。

`THIRD-PARTY-NOTICES.md` 由 G00 维护（根文件，G17 无写权限）。本文件是**给 G00 的
待办清单**，数据来自 `python -m sourceaudit notice check`，不是手抄。

直接依赖 56 个，其中 23 个未在 NOTICE 点名，已登记临时豁免
（`notice-waivers.json`，到期 2026-12-31，到期自动阻断）。

## 待补登（23 项）

| crate | 是直接依赖 | 引入方（前 3 个） | issue | 豁免到期 |
|---|---|---|---|---|
| `anyhow` | 是 | wxlike-admin, wxlike-container, wxlike-core | #228 | 2026-12-31 |
| `async-trait` | 是 | wxlike-client-core, wxlike-client-native, wxlike-client-web | #228 | 2026-12-31 |
| `bytes` | 是 | wxlike-push | #228 | 2026-12-31 |
| `cpal` | 是 | wxlike-rtc | #228 | 2026-12-31 |
| `dirs` | 是 | wxlike-admin, wxlike-server | #228 | 2026-12-31 |
| `ed25519-dalek` | 是 | wxlike-container | #228 | 2026-12-31 |
| `futures-util` | 是 | wxlike-mq, wxlike-net | #228 | 2026-12-31 |
| `http-body-util` | 是 | wxlike-push | #228 | 2026-12-31 |
| `hyper-util` | 是 | wxlike-push | #228 | 2026-12-31 |
| `js-sys` | 是 | wxlike-client-web | #228 | 2026-12-31 |
| `md-5` | 是 | wxlike-rtc | #228 | 2026-12-31 |
| `metrics` | 是 | wxlike-net | #228 | 2026-12-31 |
| `metrics-exporter-prometheus` | 是 | wxlike-net | #228 | 2026-12-31 |
| `nokhwa` | 是 | wxlike-rtc | #228 | 2026-12-31 |
| `opus` | 是 | wxlike-rtc | #228 | 2026-12-31 |
| `rustls-pki-types` | 是 | wxlike-net | #228 | 2026-12-31 |
| `serde_path_to_error` | 是 | wxlike-api, wxlike-platform | #228 | 2026-12-31 |
| `sha1` | 是 | wxlike-rtc, wxlike-server | #228 | 2026-12-31 |
| `thiserror` | 是 | wxlike-admin, wxlike-client-core, wxlike-client-native | #228 | 2026-12-31 |
| `totp-rs` | 是 | wxlike-platform | #228 | 2026-12-31 |
| `wasm-bindgen` | 是 | wxlike-client-web | #228 | 2026-12-31 |
| `wasm-bindgen-futures` | 是 | wxlike-client-web | #228 | 2026-12-31 |
| `web-sys` | 是 | wxlike-client-web | #228 | 2026-12-31 |

补登格式与既有表一致：`| crate | version | license | used for |`。许可以
`cargo metadata --format-version 1` 的结果为准，不猜。每补一项，删掉
`notice-waivers.json` 里对应条目（`NOTICE-STALE-WAIVER` 会提醒）。

## 已登记（无需补）

| 文件 | 上游 | 许可 |
|---|---|---|
| `ui/src/vendor/tweb/slicedArray.ts` | morethanwords/tweb | GPL-3.0-only |
| `ui/src/vendor/tweb/listenerSetter.ts` | morethanwords/tweb | GPL-3.0-only |
| `crates/rtc/src/sfu.rs` | 8xFF/atm0s-media-server | MIT |

细节见 `registry.json` 与 `patches/`。
