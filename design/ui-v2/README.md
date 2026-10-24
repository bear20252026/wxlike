# 最新UI设计 v2

`ui-design-v2-source.zip`为用户原始设计包；`prototype/`为可读源码镜像。10个桌面页、27个手机页。此目录是产品设计基线，不是已接服务端的生产客户端。

原始压缩包SHA256：`1d141fbf0241223201380ad91f590d45ca17fa7582b9d0b5ba7805e32f3be276`。

运行：在prototype目录执行`npm ci`、`npm run dev`；检查`npx tsc --noEmit`、`npm run build`。Node版本按Vite7要求和仓库统一工具链配置。依赖安装与运行产生的node_modules/dist不要提交。

生产实现及任务见[全平台交接目录](../../docs/full-platform-delivery/README.md)与[最新UI变更清单](../../docs/full-platform-delivery/03_ui_v2_changes.md)。

维护说明：原始ZIP保持用户上传字节不变。源码镜像将付款提示函数更名为paymentHint以修正拼写检查；没有修改设计行为。入口清单中的源码SHA已同步。
