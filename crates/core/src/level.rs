//! 认证级别枚举的共用读写。
//!
//! identity 的 `AuthLevel` 与 relations 的 `RelationLevel` 是两个**不同的
//! 类型**（令牌级别 vs 关系推导出的访问档位），不该合并成一个；但两者与
//! 上游 `auth/auth.go` 的字符串编码完全一致：`"" / anon / auth / root`。
//! 这段 parse/as_str 原本各写一份，任何一边改口径都会悄悄漂移。
//!
//! 宏只生成方法，不生成类型：调用方仍保留自己的枚举与文档，类型安全不变。

/// 为 `None/Anon/Auth/Root` 四档枚举生成 `parse` / `as_str`。
///
/// `parse` 认不出来就是 `None`，绝不猜"大概是普通用户"；`as_str` 的
/// `None` 是空串（与上游 `MarshalText` 同一条）。
#[macro_export]
macro_rules! auth_level_ops {
    ($ty:ty) => {
        impl $ty {
            /// 从令牌/头部的字符串解析级别；未知值一律 `None`。
            pub fn parse(raw: &str) -> Self {
                match raw {
                    "anon" | "ANON" => Self::Anon,
                    "auth" | "AUTH" => Self::Auth,
                    "root" | "ROOT" => Self::Root,
                    _ => Self::None,
                }
            }

            /// 编码回小写字符串；`None` 为空串。
            pub fn as_str(self) -> &'static str {
                match self {
                    Self::None => "",
                    Self::Anon => "anon",
                    Self::Auth => "auth",
                    Self::Root => "root",
                }
            }
        }
    };
}
