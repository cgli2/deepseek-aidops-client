# DeepSeek-AIOps Harness

[中文文档](README-ZH.md)

A native Rust coding-agent platform with **AIOPS Desktop**: a local-first desktop client for
tool-using and long-running engineering work. It combines an egui/eframe UI, a durable agent
runtime, workspace-scoped tools, and an extensible plugin architecture.

## Why Harness

Harness is built for teams that want a responsive local agent without an Electron or WebView
runtime. Its native Rust design keeps the agent close to the workspace while supporting durable,
auditable work in development, DevOps, AIOps, and resource-constrained environments.

## Highlights

- **Native desktop** — session history, workspace switching, Markdown rendering, model profiles,
  and encrypted local API-key storage.
- **Complete agent loop** — streaming responses, function calls, tool execution, result feedback,
  and continued reasoning in one runtime.
- **Multiple providers** — DeepSeek, OpenAI-compatible APIs, Anthropic-compatible APIs, local
  models, and offline replay mode.
- **Workspace-aware tools** — filesystem, precise editing, shell, search, planning, memory, and
  Git capabilities guarded by policies and workspace boundaries.
- **Durable long-horizon work** — append-only task DAGs, lease recovery, rate/token budgets,
  immutable artifacts, quality gates, and checkpoints for irreversible operations.
  - **LHA P2 (2025)** — 新增 `lha` 控制面模块：MVCC 工件投递与崩溃恢复、HITL 不可逆副作用精确绑定、
    契约锁（放行 body / 阻断公共签名漂移 / 防路径逃逸）、全局预算耗尽终态持久化（`BudgetExhausted`）、
    新任务波次预算回补。见 `harness-runtime/tests/lha_p2.rs`（7 个用例）与 `docs/lha-gap-analysis.md`。
- **Composable architecture** — independently combine GUI, TUI, headless, ACP, sandbox, and WASM
  capabilities through Definition / Provider / Consumer seams.

## Quick start

### Prerequisites

- Rust stable via [rustup](https://rustup.rs/)
- On Windows: Visual Studio Build Tools with the MSVC toolchain
- An API key for live models, or replay mode for offline validation

### Run from source

```bash
cd harness

# Offline replay mode — useful for smoke tests and UI development
HARNESS_REPLAY=1 cargo run -p harness-bin

# Use DeepSeek from the environment
DEEPSEEK_API_KEY=sk-... cargo run -p harness-bin
```

`config/default.toml` starts the GUI by default. Provider settings and API keys can also be saved
in the desktop app's Model Settings page. In PowerShell, set environment variables with
`$env:NAME = "value"` before running Cargo.

### Common commands

```bash
make help                 # list available commands
make dev                  # start the desktop app
make dev-replay           # start with the replay provider
make dev-watch            # rebuild and restart on UI/entry changes
make check                # check the full workspace
make test                 # run the full workspace test suite
```

On Windows, use the supplied build script; it configures the MSVC target:

```bat
cd harness
scripts\build.bat check -p harness-bin
scripts\build.bat package
```

## Long-horizon tasks

Long-horizon control-plane state is stored under `<workspace>/.harness/long-horizon/`.

```bash
cd harness
cargo run -p harness-bin -- lh status
cargo run -p harness-bin -- lh submit "Build, test, and fix all failures"
cargo run -p harness-bin -- lh approve <checkpoint-id> "Reviewed the staged artifact"
cargo run -p harness-bin -- lh reject <checkpoint-id> "Risk is not acceptable"
```

Interactive tasks have **no default wall-clock deadline**. Cancellation, process shutdown, and
worker leases handle recovery. To enforce an operational deadline, set a positive
`HARNESS_TURN_TIMEOUT_SECS`; leave it unset or set it to `0` for no deadline.

## Architecture

| Layer | Primary crates | Responsibility |
| --- | --- | --- |
| Microkernel | `harness-core` | typed context, events, plugins, workspace, and configuration |
| Domain primitives | `harness-llm`, `harness-session` | provider contracts and append-only session truth |
| Capability seams | `harness-capability` | trait-only definitions for tools and integrations |
| Providers | `harness-provider-*` | local, sandbox, WASM, memory, Git, hooks, and other implementations |
| Runtime | `harness-runtime`, `harness-tool` | agent loop, scheduling, governance, and model-visible tools |
| Boundary and UI | `harness-acp`, `harness-sdk`, `harness-ui`, `bin` | ACP, SDK, presentation, and composition |

Session events are the source of truth; consumers depend on capability traits rather than concrete
providers; the UI consumes events instead of driving the agent loop. See the
[architecture guide](docs/architecture.md) for diagrams and details.

## Security and data

- File and editing operations are scoped to the selected workspace.
- Shell access is policy-governed; use hooks or a restricted account/container for untrusted work.
- WASM providers have no direct host access unless a capability bridge is explicitly enabled.
- Desktop API keys are encrypted with AES-256-GCM in the local settings database.
- Sessions live in `<workspace>/.harness/sessions/`; long-horizon state lives in
  `<workspace>/.harness/long-horizon/`.

## Documentation

- [Chinese README](README-ZH.md)
- [System design](docs/system-design-completion.md)
- [Architecture and dependency diagrams](docs/architecture.md)
- [Extension cookbook](harness/extensions/EXTENSION-COOKBOOK.md)
- [Long-horizon orchestration design](docs/Long-Horizon%20Autonomous%20Orchestration%20Architecture.md)

## License

Licensed under the [MIT License](LICENSE). Copyright © 2026 cgli.
