# `Platform` 公开面收缩

`Platform` 是组合根，不是业务服务。`impl Platform` 里的方法默认应只给
`wxlike-platform` 内的 handler 用；只有真正跨 crate 的入口才保留 `pub`。

## 盘点工具

```bash
python scripts/platform_surface_audit.py
python scripts/platform_surface_audit.py --json
```

口径：

- 只统计 `impl Platform` 块内的方法，混不进 free function / 其它 impl；
- “外部调用”指 `crates/platform` 之外的 Rust 文件里出现 `.method(` 或
  `Platform::method(`；
- 同名方法可能来自别的类型，脚本会给出位置，最终以编译结果为准。

## 当前基线

| 时点 | `Platform` 方法总数 | `pub` | `pub(crate)` |
|---|---:|---:|---:|
| 第一批前 | 285 | 258 | 27 |
| 第一批后 | 285 | **232** | **53** |

目标：`pub` 方法 ≤120。

## 第一批：status / favorites / notes

这三个模块的 26 个方法只被平台内 handler 调用，已从 `pub` 降为
`pub(crate)`：

- `domain/status.rs`：12 个（`friend_ids` 到 `sweep_expired_statuses`）；
- `domain/favorites.rs`：9 个；
- `domain/notes.rs`：5 个。

`status` / `note` 两个通用方法名在外部搜索里有命中，但分别来自
`http::Response::status()` 与 `message::sync::note()`，不是 `Platform` 调用，
已人工核对。

## 后续约定

1. 新增 `Platform` 方法默认 `pub(crate)`，需要跨 crate 时才加 `pub`；
2. 每批先跑盘点脚本，再改可见性，最后用 CI 编译兜底；
3. 下一批优先 `articles / moments / media / video_feed` 等纯 handler 模块；
4. 目标不是把方法藏起来，而是把真正的对外面收敛成 Account / Session /
   Message / Media / Wallet / Governance / Realtime / Health 几个 façade。
