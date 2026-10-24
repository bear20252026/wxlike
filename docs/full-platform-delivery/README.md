# wxlike 全平台开发交接包

基线：`d0bef13ad60c1f8b56a741789aab93c66026ae8b`；研究日期：2026-10-24。

范围：Win/macOS/Linux桌面、网页、Android/iOS；一套Rust服务端（Linux生产/Docker、Win/Mac可跑）；管理后台、小程序容器、MCP/CLI；共享Rust业务核心、统一云CI。目标是UI每个产品入口实现字面功能，并有持久化、权限、错误恢复和跨端证据。

## 阅读顺序

1. `00_master_guide.md`：范围、架构ADR、各组所有权、协议、安全、CI与合并次序。
2. `01_contracts_and_data.md`：具体模块结构、表约束、API族、跨域事务/补偿和测试场景。
3. `02_staffing_and_distance.md`：目前差距、8–12人职责编排、208–378开发人周粗估与里程碑；不能当固定报价。
4. `teams/G00.md`到`teams/G17.md`：18组100卡，每卡目标路径、接口、具体实现、验收、交接及上游参考。
5. `references.md`与`upstream_manifest.json`：用户8仓及新增成熟项目；29个远端完整SHA研究锚点、许可边界与直接依赖/移植/参考分类。

## 分派与追踪

- `task_board.csv`：可筛选分派；`tasks.json`含实现正文、hard_dependencies与integration_dependencies。硬依赖图已校验无环；集成依赖允许ports/SDK间存在协作循环，用先冻结契约和fixture打断等待，不能误当所有组必须串行。
- `owners.json`：计划的互斥目录所有权；G00-02落实到真实仓库后生成CODEOWNERS。各域契约、迁移草稿、集成请求亦分owner。当前是发包设计，不是已执行的分支保护规则。
- `entry_seed.csv/json`：上传UI的331个onClick语法位置，附源文件行列、组件、handler及hash、初始owner/task。自动分类是启发式，必须人工复核；map/菜单/长按/onChange/键盘/无handler图标还要在G00-01展开为正式运行时入口总账。这份文件没有宣称331项就是全部功能。
- `provider-register.csv`：17类外部/基础设施依赖，尚未替用户选择或开通供应商；真实支付/票务/医疗等缺证据时保持阻塞。
- `templates/`：PR、集成请求、action与入口记录模板。
- `validation_report.json`：本交接包ID/依赖/归属/种子hash的一致性验证，不能当作产品功能或CI验收通过。

第一批先发G00-01/02/03，G17-01同步做来源审查，再开G01-01。不要100卡同时从旧主干开始改巨型文件。每人取自己的组文件；跨组只用已冻结ports和fixture。一个人可以兼组，但同一时刻不得有两个owner写同一路径。

## 事实与限制

这是可实施的开发方案与任务包，本次交付文档与最新UI设计，并发布GitHub任务；不等于业务功能已实现，也未替用户指定真人负责人。旧项目可复用内容已纳入，不重复做已完成的ureq 3迁移。没有保证零合并冲突或“完美”性能平衡；采用单owner、串行公共改动、契约版本、merge queue和实际测试降低返工。

许可证元数据只是筛选，实际引入需逐文件核查；AGPL代码可按本项目既有AGPL许可策略审核移植，MIT/Apache也须保留版权和NOTICE。安全公告在依赖冻结时重查。iOS签名/审核、推送和第三方资金/服务联调不能由代码指南自行完成。


## 当前版本与任务入口

当前UI v2有10个桌面页、27个手机页；新增/扩展任务详见[UI变更](03_ui_v2_changes.md)。[GitHub任务总索引](issues/index.md)包含18组、100张任务卡与真实Issue链接；任务以Issue状态为准，不从原型演示推定完成。

原版总册和ZIP在archives；当前总册为wxlike_full_development_guide-v2.md，当前打包为wxlike_team_handoff-v2.zip。
