# H-02/H-03 关闭记录：scope 交集收窄与 glob 后缀

- 审计基线：`60895e18`
- 结论：**H-02、H-03 关闭**。

## H-02 命令 scope 交集放大授权

原实现 `ScopeSet::intersect` 只要两个 selector “有重叠”就保留左侧原值：
`conv:*` ∩ `conv:g-1` 仍得到 `conv:*`，受限 grant 收不窄宽 token。

修复 `crates/cmd/src/auth.rs::selector_intersection`：

- `*` ∩ 具体 selector → 具体 selector；
- 精确 selector ∩ glob → 精确值（用 `glob_match` 验证包含）；
- 两个 `prefix*` → 取更长（更窄）前缀；
- 两个复杂 glob 算不出精确交集 → 空集（保守拒绝，不猜授权）。

`intersect` 现在写入语义交集并去重；代理链 `token ∩ parent ∩ granted` 只减不增。

## H-03 glob matcher 后缀预留错误

原 `glob_match` 校验末段后缀后没有从 `rest` 剥掉末段，导致 `a*b*bc` 能匹配 `abc`（中间 `*` 重复消费了 `bc`）。

修复 `crates/cmd/src/policy.rs`：先 `strip_suffix(last)`，再让中间段在剩余串上按顺序查找；同时要求首段前缀、末段后缀都落在同一 `rest` 上。

## 验证

```bash
cargo test -p wxlike-cmd --lib
cargo test -p wxlike-cmd
```

新增：
- `scope_intersection_narrows_wildcards_instead_of_keeping_the_token`：`conv:*` ∩ `conv:g-1` 只能允许 g-1；
- `scope_intersection_handles_prefix_containment_and_disjoint`：`team-*` ∩ `team-a*` → `team-a*`，与 `other-*` 无交集；
- `glob_reserves_the_suffix_before_scanning_middle_segments`：`a*b*bc` 不匹配 `abc`、匹配 `abbc`，`a*a` 不匹配单 `a`。

## 边界

两个复杂 glob 的交集不尝试自动机求交，直接保守为空；这会拒绝本可允许的极少数组合，但不会放大授权。若以后 scope 语法扩展到复杂 glob，再引入精确的 glob 交集算法。
