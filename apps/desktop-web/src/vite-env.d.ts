/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_APP_VERSION?: string;
  readonly VITE_GIT_SHA?: string;
  readonly VITE_BUILD_TIME?: string;
  readonly VITE_CORE_FIXTURE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
