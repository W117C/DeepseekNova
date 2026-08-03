/// <reference types="vite/client" />

// Vite worker 导入（opencode markdown 高亮使用 web worker）
declare module "*?worker&url" {
  const src: string
  export default src
}
declare module "*?worker" {
  const workerConstructor: {
    new (options?: { name?: string }): Worker
  }
  export default workerConstructor
}

// 资源导入
declare module "*.woff2" {
  const src: string
  export default src
}
declare module "*.ttf" {
  const src: string
  export default src
}
declare module "*.svg" {
  const src: string
  export default src
}