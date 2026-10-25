# UP-TWEB · `src/helpers/slicedArray.ts` → `ui/src/vendor/tweb/slicedArray.ts`

- 上游：`morethanwords/tweb` @ `57ef3edec3c124efa98a40dba29f3138eef187fc`
  永久链接：<https://github.com/morethanwords/tweb/blob/57ef3edec3c124efa98a40dba29f3138eef187fc/src/helpers/slicedArray.ts>
- 许可：GPL-3.0-only（与本项目 AGPL-3.0-only 兼容）
- 本地 sha256：`cc2dadd07bf0c4b3d69a371f2226a6e91635a0670118eac6b7ff1689835385c2`

## 拿了什么

降序分块数组：`SlicedArray` + `Slice` 的 `ends` 追踪、`insertSlice` 的合并逻辑。
这是消息列表虚拟化的核心——跨页去重、乱序插入都靠 `ends` 语义。

## 改了什么（三处，均写在文件头）

1. 去掉 `MOUNT_CLASS_TO` 调试挂载（我们不需要全局类注册）；
2. 内联 `indexOfAndSplice` 与 number-only 的 `compareValue`（少两层间接调用）；
3. 按本项目 `tsconfig` 的 `strict` 补变量初始化。

## 没拿什么

tweb 的 root 挂载、`App` 侧生命周期钩子、以及其消息索引的其余 helper。粘性边界是"能算切片"为止。

## 复核方式

`python -m sourceaudit provenance check` 会核对本地 sha256；改动本文件必须同时更新
`registry.json` 的 `local_sha256`，否则门禁红。

## 2026-10-25 换行复核

旧指纹来自 Windows CRLF 工作区。已逐字节验证：仓库 LF 内容仅将换行转换为 CRLF 后即匹配旧指纹；代码内容无变化。现登记 Git 中 LF 字节的 sha256，`.gitattributes` 固定该文件以 LF 检出，保证 Linux / Windows 复核一致。
