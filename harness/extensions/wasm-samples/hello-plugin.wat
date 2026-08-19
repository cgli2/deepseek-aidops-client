;; AIOps Desktop 最小 WASM 插件示例。
;;
;; 导入后会调用 on_load；禁用或移除时会调用 on_unload。
;; 它只使用 env.host_log，不具备 Shell、文件系统或网络权限。
(module
  (import "env" "host_log" (func $host_log (param i32 i32)))
  (memory (export "memory") 1)

  (data (i32.const 0) "hello-plugin loaded")
  (data (i32.const 64) "hello-plugin unloaded")

  (func (export "on_load")
    (call $host_log (i32.const 0) (i32.const 19)))

  (func (export "on_unload")
    (call $host_log (i32.const 64) (i32.const 21)))
)
