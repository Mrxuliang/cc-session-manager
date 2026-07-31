# CC Session Manager

管理 Claude Code 会话的 Mac 桌面应用（Tauri 2 + Vue 3 + TypeScript + Rust），MIT 协议开源。

> A macOS desktop app to manage Claude Code sessions: see the exact context-token
> size of every session, and carry old sessions into a new one — full fork,
> AI-distilled handoff summary, or hand-picked fragments — launched in
> Claude Desktop (deep links) or Terminal.

核心解决两件事：

1. **精准管理**：扫描 `~/.claude/projects/*/*.jsonl`，按项目分组展示所有历史会话，
   每个会话直接标出**当前上下文 token 规模**（取最后一条 assistant 的
   `input + cache_read + cache_creation`，精确值非估算）、消息数、文件大小、分支、模型。
   支持搜索、按上下文大小排序、查看完整对话记录。
2. **一键把旧会话带入新会话**，三种模式：
   - **完整分叉**：`claude --resume <id> --fork-session`，无损带全部历史（起步成本 = 原上下文）；
   - **摘要蒸馏**（省 token）：后台 `claude -p --no-session-persistence` 把选中的一个或
     多个会话蒸馏成交接摘要，可编辑后作为新会话首条消息注入；
   - **手写 / 片段**：在详情页勾选若干轮对话，只带这些片段（或手写背景）进新会话。

  新会话可选两种启动目标（向导底部切换，默认 Desktop）：
   - **Claude Desktop**：摘要/片段走 `claude://code/new?q=<文本>&folder=<目录>` 深链
     （q 上限 14336 字符，Desktop 内部常量 `16384-2048`）；完整带入则把旧 jsonl
     复制成新 UUID（逐行改写 sessionId，原文件不动）后走 `claude://resume?session=<id>`
     导入，等效分叉。列表卡片上也有「Desktop ↗」按钮直接导入打开原会话。
   - **Terminal**：AppleScript 打开 Terminal 跑 `claude`，无长度限制。

## 开发

```bash
npm install
npm run tauri dev
```

## 打包

```bash
npm run tauri build
# 产物: src-tauri/target/release/bundle/macos/cc-session-manager.app
```

## 结构

- `src-tauri/src/lib.rs` — 全部后端逻辑：
  - `scan_sessions` 扫描索引（mtime+size 内存缓存，增量）
  - `read_session` 解析单个会话为可读消息流（过滤 sidechain/meta/噪音）
  - `generate_digest` 拼转写 → 调 `claude -p` 蒸馏（超长时保留头 1/8 + 尾 7/8）
  - `launch_session` 写临时脚本 → osascript 打开 Terminal 启动 claude
- `src/App.vue` — 全部 UI：项目侧栏 / 会话列表 / 详情抽屉（片段勾选）/ 带入向导（token 成本预览）

## 注意

- jsonl 永远只读，不写不动 Claude Code 的任何数据；
- 临时脚本与蒸馏交接文件写在 `~/.cc-session-manager/`；
- 依赖本机已安装并登录的 `claude` CLI（通过 `/bin/zsh -lc` 调用以获得 PATH）。
