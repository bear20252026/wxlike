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
| 第一批后 | 285 | **237** | **48** |
| 第二批后 | 282 | **194** | **88** |

目标：`pub` 方法 ≤120。

## 第一批：status / favorites / notes

这三个模块的 26 个方法只被平台内 handler 调用，其中 **21 个**已从 `pub` 降为
`pub(crate)`：

- `domain/status.rs`：11 个（`friend_ids` 到 `remove_status_background` 之间）；
- `domain/favorites.rs`：5 个（`add_object_favorite`、`favorites_sorted`、
  `search_favorites`、`update_favorite`、`reorder_favorites`）；
- `domain/notes.rs`：5 个。

保留 `pub` 的 5 个是平台集成测试直接调用的方法：
`add_favorite / list_favorites / favorite / remove_favorite /
sweep_expired_statuses`。它们要等测试改用 HTTP 面或迁回 crate 内单测后才能
降级，不能为了数字直接断测试。

`status` / `note` 两个通用方法名在外部搜索里有命中，但分别来自
`http::Response::status()` 与 `message::sync::note()`，不是 `Platform` 调用，
已人工核对；盘点脚本现在也把 `crates/platform/tests` 算作外部调用。

## 第二批：articles / moments / video_feed

43 个只被平台内 handler 与 HTTP 测试调用的方法从 `pub` 降为
`pub(crate)`（这三个模块里另有 1 个方法原本已是 `pub(crate)`，合计 44 个）：

- `domain/articles.rs`：13 个；
- `domain/moments.rs`：18 个；
- `domain/video_feed.rs`：13 个。

`crates/platform/tests` 没有直接调用这些方法，HTTP 面测试不受影响。

顺带删掉三个已经无人调用的包装方法：`publish_followers`、`publish_moment`、
`can_see_moment`，以及只服务 `publish_followers` 的 repo 函数
`publisher_followers`。它们变成 `pub(crate)` 后触发 dead_code，正好证明
“留着 public 只是为了不报错”，不如直接删。

## 后续约定

1. 新增 `Platform` 方法默认 `pub(crate)`，需要跨 crate 时才加 `pub`；
2. 每批先跑盘点脚本，再改可见性，最后用 CI 编译兜底；
3. 下一批优先 `articles / moments / media / video_feed` 等纯 handler 模块；
4. 目标不是把方法藏起来，而是把真正的对外面收敛成 Account / Session /
   Message / Media / Wallet / Governance / Realtime / Health 几个 façade。
