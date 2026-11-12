//! **仓储层**（`security`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。
//!
//! 本 feature 的 SQL 目前还写在 [`crate::domain::security`] 的 `impl Platform` 里
//! （与 `users` / `sessions` / `notes` 一样：单条语句短、与业务判定交织，抽出来
//! 只会让读代码的人来回跳文件）。这里留一个空壳，保持 domain 与 repo 的文件
//! 一一对应——以后哪条语句长到需要单独测试，就落在这里。
