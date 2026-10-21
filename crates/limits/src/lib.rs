//! 跨 crate 的数值约定：**按含义命名，不按值命名**。
//!
//! 审计里最刺眼的一条是"同一个 `64` 有三种含义"：sha256 的十六进制长度、
//! 贴纸标签的字数上限、签名数组的长度。它们碰巧都等于 64，于是复制粘贴时
//! 谁也不会觉得不对——直到有人把 hash 换成 sha512，改了其中一处，另外两处
//! 跟着"变得看起来对"，而真正该改的那处没改。
//!
//! # 为什么单独成一个 crate
//!
//! 这些常量本该放在 `wxlike-core`。但 `core` 带着 `rusqlite`（bundled SQLite）
//! 与 `vodozemac`，而 `wxlike-client`（CLI / MCP 的底座）只需要两个分页默认值——
//! 为了两个数字把 SQLite 编进命令行工具，是拿构建时间换"看起来更内聚"。
//! 所以拆出一个**零依赖**的 crate，谁需要谁依赖，`core` 再 `pub use` 回去，
//! 老的 `wxlike_core::limits::...` 路径仍然可用。
//!
//! 这里只放**跨 crate 共用**的约定。只在一个模块里用到的上限，就留在那个模块
//! 里当 `const`——这个模块变成"什么数字都往里塞"的垃圾桶，比魔法数字更糟。

/// sha256 的十六进制表示长度（`media_id` / `video_id` / `sha256` 字段）。
///
/// 32 字节 → 64 个十六进制字符。这个值一旦变化，所有"id 长度必须等于 N"
/// 的校验都要跟着变，所以它只能有一个定义。
pub const SHA256_HEX_LEN: usize = 64;

/// 列表/历史的默认分页大小。
///
/// 50 是"一屏半"的经验值：太小要频繁翻页，太大一次序列化就变重。
pub const DEFAULT_PAGE_LIMIT: u64 = 50;

/// 搜索的默认条数。比分页小——搜索是"找到那一条"，不是"翻一页看看"。
pub const DEFAULT_SEARCH_LIMIT: u64 = 20;

/// 对称密钥/随机标识的字节数（32 字节 = 256 位）。
pub const KEY_BYTES: usize = 32;

// 这三条用**编译期断言**而不是测试：条件全是常量，写成 `#[test]` 只会在运行时
// 做一次恒真判断（clippy 1.99 会直接报 `assertions_on_constants`）。
// 放在模块级 `const _` 里，编译不过就是编译不过。
const _: () = assert!(SHA256_HEX_LEN == 32 * 2, "hex 长度 = 32 字节 × 2");
const _: () = assert!(KEY_BYTES == 32, "对称密钥 32 字节 = 256 位");
const _: () = assert!(
    DEFAULT_SEARCH_LIMIT < DEFAULT_PAGE_LIMIT,
    "搜索默认条数应当小于一页"
);
