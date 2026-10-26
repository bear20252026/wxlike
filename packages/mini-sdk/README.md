# @wxlike/mini-sdk

小程序（guest 侧）的类型化 SDK：把 `mini-host.v1` 桥协议包成
`createClient()` + 每个 method 一个异步函数。运行时零依赖（纯 ESM JavaScript，
Node ≥ 20 / 现代浏览器），宿主桥的语义（request_id、错误码、scope）全部来自
契约，不在这里手写。

## 目录

- `src/index.js` + `src/types.d.ts` —— 手写部分：`createClient`、transport
  抽象（调用方注入 fetch/postMessage/…）、`MiniHostError`。
- `src/generated.js` + `src/generated.d.ts` —— **生成物，不要手改**。

## generate.mjs 怎么工作

`npm run generate`（`scripts/generate.mjs`）读取三个契约文件：

- `contracts/domains/miniapps/mini-host.v1.json`（`oneOf` 分支 = 桥 method）
- `contracts/domains/miniapps/mini-catalog.v1.json`
- `contracts/domains/miniapps/mini-package.v1.json`

从每个分支的 `title`（method 名）和 `x-scope` 生成 METHOD/SCOPE 表与
`x-contract-version` 常量，写进 `src/generated.*`。重名 method、缺版本号、
缺 scope 都会直接报错退出——生成物与契约不可能悄悄漂移。

## 契约一致性测试

`npm test`（`node --test`，无需安装依赖）里 `test/sdk.test.mjs` 会**重读
mini-host.v1.json**，逐条比对生成的 method/scope/错误码表；任何契约改动没
重新 generate 就会红。这组测试同时跑在 CI 的 `web-v2` 任务里
（`ci/jobs/build/web_v2.job.yml`）。

## 类型检查

`npm run typecheck`（`tsc --noEmit`，`tsconfig.json` 只收 `src/` 的声明文
件）：手写与生成的 `.d.ts` 必须能一起通过 strict 解析。需要先
`npm install` 装一次 devDependencies 里的 typescript。
