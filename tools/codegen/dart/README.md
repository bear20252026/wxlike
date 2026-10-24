# Dart 绑定生成（G01-04 / G01-05）

`CoreHandle` 的 Dart 绑定由 UniFFI 从**编译产物**里读接口形状生成，不手写、
不手改。

```bash
# 生成到 apps/mobile/lib/src/rust/（G03 的目录，交接时由 G03 消费）
python tools/codegen/dart/gen_dart.py

# 只检查：重新生成到临时目录，与仓库逐字节比对
python tools/codegen/dart/gen_dart.py --check
```

两条约束：

1. **生成物不提交**（`.gitignore` 里已排除 `apps/mobile/lib/src/rust`），
   CI 每次重新生成——和 `ui/dist/app.js` 同一个口径。
2. 生成的 `manifest.txt` 带契约摘要（`contract-v1 digest=…`），Dart 侧可以
   断言"绑定与契约同源"；摘要对不上说明该重跑 `tools/codegen/generate.py`。
