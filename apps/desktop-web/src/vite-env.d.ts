/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_APP_VERSION?: string;
  readonly VITE_GIT_SHA?: string;
  readonly VITE_BUILD_TIME?: string;
  readonly VITE_CORE_FIXTURE?: string;
  /** 浏览器端 WASM 核心的服务端基址（G01-03）；缺省回退当前 origin。 */
  readonly VITE_CORE_BASE_URL?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
