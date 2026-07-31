<script setup lang="ts">
import { ref, computed, onMounted } from "vue";
import { invoke } from "@tauri-apps/api/core";

interface SessionMeta {
  session_id: string;
  path: string;
  project_slug: string;
  cwd: string;
  git_branch: string;
  title: string;
  custom_title: boolean;
  first_ts: string;
  last_ts: string;
  user_msgs: number;
  assistant_msgs: number;
  file_size: number;
  context_tokens: number;
  total_output_tokens: number;
  model: string;
}
interface MsgView {
  index: number;
  role: string;
  text: string;
  ts: string;
}

const sessions = ref<SessionMeta[]>([]);
const loading = ref(false);
const error = ref("");
const search = ref("");
const activeProject = ref<string>("__all__");
const sortBy = ref<"time" | "tokens">("time");
const selected = ref<Set<string>>(new Set());

// 详情
const detail = ref<SessionMeta | null>(null);
const detailMsgs = ref<MsgView[]>([]);
const detailLoading = ref(false);
const pickedMsgs = ref<Set<number>>(new Set());

// 向导
const wizardOpen = ref(false);
const wizardMode = ref<"fork" | "digest" | "manual">("digest");
const wizardCwd = ref("");
const digestText = ref("");
const digesting = ref(false);
const digestElapsed = ref(0);
let digestTimer: number | undefined;
const launching = ref(false);
const wizardErr = ref("");
const target = ref<"desktop" | "terminal">(
  (localStorage.getItem("cc-target") as "desktop" | "terminal") || "desktop"
);
function setTarget(t: "desktop" | "terminal") {
  target.value = t;
  localStorage.setItem("cc-target", t);
}
// Desktop 深链 q 参数上限（Claude.app 内部常量 16384-2048）
const DESKTOP_PROMPT_MAX = 14000;

async function scan() {
  loading.value = true;
  error.value = "";
  try {
    sessions.value = await invoke<SessionMeta[]>("scan_sessions");
  } catch (e: any) {
    error.value = String(e);
  } finally {
    loading.value = false;
  }
}
onMounted(scan);

function projName(s: SessionMeta): string {
  const c = s.cwd || s.project_slug;
  const seg = c.split("/").filter(Boolean);
  return seg.length ? seg[seg.length - 1] : c;
}
const projects = computed(() => {
  const map = new Map<string, { name: string; count: number }>();
  for (const s of sessions.value) {
    const key = s.cwd || s.project_slug;
    const e = map.get(key) || { name: projName(s), count: 0 };
    e.count++;
    map.set(key, e);
  }
  return [...map.entries()].sort((a, b) => b[1].count - a[1].count);
});

const filtered = computed(() => {
  let list = sessions.value;
  if (activeProject.value !== "__all__")
    list = list.filter((s) => (s.cwd || s.project_slug) === activeProject.value);
  const q = search.value.trim().toLowerCase();
  if (q)
    list = list.filter(
      (s) =>
        s.title.toLowerCase().includes(q) ||
        s.cwd.toLowerCase().includes(q) ||
        s.session_id.includes(q)
    );
  if (sortBy.value === "tokens")
    list = [...list].sort((a, b) => b.context_tokens - a.context_tokens);
  return list;
});

const selectedSessions = computed(() =>
  sessions.value.filter((s) => selected.value.has(s.session_id))
);

function toggleSelect(id: string) {
  const n = new Set(selected.value);
  n.has(id) ? n.delete(id) : n.add(id);
  selected.value = n;
}

// ---------- 格式化 ----------
function fmtTokens(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n >= 1000) return (n / 1000).toFixed(n >= 100_000 ? 0 : 1) + "k";
  return String(n);
}
function fmtSize(n: number): string {
  if (n >= 1 << 20) return (n / (1 << 20)).toFixed(1) + " MB";
  if (n >= 1024) return (n / 1024).toFixed(0) + " KB";
  return n + " B";
}
function fmtTime(ts: string): string {
  if (!ts) return "";
  const d = new Date(ts);
  const diff = Date.now() - d.getTime();
  if (diff < 3600_000) return Math.max(1, Math.floor(diff / 60000)) + " 分钟前";
  if (diff < 86400_000) return Math.floor(diff / 3600_000) + " 小时前";
  if (diff < 7 * 86400_000) return Math.floor(diff / 86400_000) + " 天前";
  return `${d.getMonth() + 1}-${String(d.getDate()).padStart(2, "0")}`;
}
function tokenClass(n: number): string {
  if (n < 30_000) return "tok-green";
  if (n < 80_000) return "tok-yellow";
  if (n < 150_000) return "tok-orange";
  return "tok-red";
}
function shortModel(m: string): string {
  return m.replace(/^claude-/, "").replace(/-\d{8}$/, "");
}
// 粗略 token 估算：中文 ~1.5字/token，其他 ~4字符/token
function estimateTokens(s: string): number {
  const cjk = (s.match(/[一-鿿]/g) || []).length;
  return Math.round(cjk / 1.5 + (s.length - cjk) / 4);
}

// ---------- 详情 ----------
async function openDetail(s: SessionMeta) {
  detail.value = s;
  detailMsgs.value = [];
  pickedMsgs.value = new Set();
  detailLoading.value = true;
  try {
    detailMsgs.value = await invoke<MsgView[]>("read_session", { path: s.path });
  } catch (e: any) {
    error.value = String(e);
  } finally {
    detailLoading.value = false;
  }
}
function togglePick(i: number) {
  const n = new Set(pickedMsgs.value);
  n.has(i) ? n.delete(i) : n.add(i);
  pickedMsgs.value = n;
}
const pickedText = computed(() => {
  if (!detail.value) return "";
  return detailMsgs.value
    .filter((m) => pickedMsgs.value.has(m.index))
    .map((m) => (m.role === "user" ? "【用户】\n" : "【助手】\n") + m.text)
    .join("\n\n");
});

// ---------- 向导 ----------
function openWizard(from: "list" | "fragments") {
  wizardErr.value = "";
  digestText.value = "";
  if (from === "fragments" && detail.value) {
    selected.value = new Set([detail.value.session_id]);
    wizardMode.value = "manual";
    digestText.value = `以下片段摘录自历史会话「${detail.value.title}」：\n\n${pickedText.value}`;
    detail.value = null;
  } else {
    wizardMode.value = selectedSessions.value.length === 1 ? "fork" : "digest";
  }
  wizardCwd.value = selectedSessions.value[0]?.cwd || "";
  wizardOpen.value = true;
}

const forkCost = computed(() =>
  selectedSessions.value.reduce((a, s) => Math.max(a, s.context_tokens), 0)
);
const digestCost = computed(() => estimateTokens(digestText.value) + 600);

async function runDigest() {
  digesting.value = true;
  wizardErr.value = "";
  digestElapsed.value = 0;
  digestTimer = window.setInterval(() => digestElapsed.value++, 1000);
  try {
    const arg = selectedSessions.value.map((s) => [s.title, s.path]);
    digestText.value = await invoke<string>("generate_digest", { sessions: arg });
  } catch (e: any) {
    wizardErr.value = String(e);
  } finally {
    digesting.value = false;
    if (digestTimer) clearInterval(digestTimer);
  }
}

async function launch() {
  launching.value = true;
  wizardErr.value = "";
  try {
    const prompt =
      "以下是从历史会话带入的背景上下文。请通读后用一句话确认你已了解背景，然后等待我的具体任务，不要自行开始做事。\n\n---\n\n" +
      digestText.value;
    if (target.value === "desktop") {
      if (wizardMode.value === "fork") {
        await invoke("fork_in_desktop", { path: selectedSessions.value[0].path });
      } else {
        if (prompt.length > DESKTOP_PROMPT_MAX) {
          wizardErr.value = `注入文本 ${prompt.length} 字符，超过 Desktop 深链上限（约 ${DESKTOP_PROMPT_MAX}），会被截断。请精简内容，或切换到 Terminal 启动（无长度限制）。`;
          launching.value = false;
          return;
        }
        await invoke("new_desktop_session", {
          cwd: wizardCwd.value,
          prompt,
        });
      }
    } else {
      if (wizardMode.value === "fork") {
        await invoke("launch_session", {
          cwd: wizardCwd.value,
          mode: "fork",
          arg: selectedSessions.value[0].session_id,
        });
      } else {
        await invoke("launch_session", {
          cwd: wizardCwd.value,
          mode: "prompt",
          arg: prompt,
        });
      }
    }
    wizardOpen.value = false;
    selected.value = new Set();
  } catch (e: any) {
    wizardErr.value = String(e);
  } finally {
    launching.value = false;
  }
}

async function openDesktop(s: SessionMeta) {
  try {
    await invoke("open_in_desktop", { sessionId: s.session_id });
  } catch (e: any) {
    error.value = String(e);
  }
}

const canLaunch = computed(() => {
  if (wizardMode.value === "fork") return selectedSessions.value.length === 1;
  return digestText.value.trim().length > 0;
});

const totalContextTokens = computed(() =>
  sessions.value.reduce((a, s) => a + s.context_tokens, 0)
);
</script>

<template>
  <div class="app">
    <!-- 侧栏：项目 -->
    <aside class="sidebar">
      <div class="brand"><span class="brand-dot"></span> CC Session Manager</div>
      <div
        class="proj"
        :class="{ active: activeProject === '__all__' }"
        @click="activeProject = '__all__'"
      >
        <span class="proj-name">全部会话</span>
        <span class="proj-count">{{ sessions.length }}</span>
      </div>
      <div
        v-for="[key, p] in projects"
        :key="key"
        class="proj"
        :class="{ active: activeProject === key }"
        :title="key"
        @click="activeProject = key"
      >
        <span class="proj-name">{{ p.name }}</span>
        <span class="proj-count">{{ p.count }}</span>
      </div>
      <div class="sidebar-foot">
        历史上下文合计 ≈ {{ fmtTokens(totalContextTokens) }} tok
      </div>
    </aside>

    <!-- 主区 -->
    <main class="main">
      <div class="toolbar">
        <input v-model="search" class="search" placeholder="搜索标题 / 路径 / ID…" />
        <select v-model="sortBy" class="sel">
          <option value="time">按时间</option>
          <option value="tokens">按上下文大小</option>
        </select>
        <button class="btn ghost" @click="scan" :disabled="loading">
          {{ loading ? "扫描中…" : "刷新" }}
        </button>
        <button
          class="btn primary"
          :disabled="selected.size === 0"
          @click="openWizard('list')"
        >
          带入新会话 ({{ selected.size }})
        </button>
      </div>
      <div v-if="error" class="error">{{ error }}</div>

      <div class="list">
        <div
          v-for="s in filtered"
          :key="s.session_id"
          class="card"
          :class="{ picked: selected.has(s.session_id) }"
        >
          <input
            type="checkbox"
            class="cb"
            :checked="selected.has(s.session_id)"
            @click.stop="toggleSelect(s.session_id)"
          />
          <div class="card-body" @click="openDetail(s)">
            <div class="card-title">
              <span v-if="s.custom_title" class="star" title="自定义标题">★</span>
              {{ s.title }}
            </div>
            <div class="card-meta">
              <span class="badge" :class="tokenClass(s.context_tokens)"
                >{{ fmtTokens(s.context_tokens) }} tok</span
              >
              <span>{{ fmtTime(s.last_ts) }}</span>
              <span>{{ s.user_msgs }} 问 / {{ s.assistant_msgs }} 答</span>
              <span>{{ fmtSize(s.file_size) }}</span>
              <span v-if="s.git_branch" class="branch">⎇ {{ s.git_branch }}</span>
              <span v-if="s.model" class="model">{{ shortModel(s.model) }}</span>
              <span class="cwd">{{ projName(s) }}</span>
            </div>
          </div>
          <button
            class="btn ghost sm desk-btn"
            title="在 Claude Desktop 中打开此会话"
            @click.stop="openDesktop(s)"
          >
            Desktop ↗
          </button>
        </div>
        <div v-if="!loading && filtered.length === 0" class="empty">没有匹配的会话</div>
      </div>
    </main>

    <!-- 详情抽屉 -->
    <div v-if="detail" class="drawer-mask" @click.self="detail = null">
      <div class="drawer">
        <div class="drawer-head">
          <div>
            <div class="drawer-title">{{ detail.title }}</div>
            <div class="drawer-sub">
              {{ detail.session_id }} · {{ fmtTokens(detail.context_tokens) }} tok ·
              {{ detail.cwd }}
            </div>
          </div>
          <button class="btn ghost" @click="detail = null">关闭</button>
        </div>
        <div class="drawer-tools">
          <span class="hint">点击对话卡片选中片段，可只带这些片段进新会话</span>
          <button
            class="btn primary sm"
            :disabled="pickedMsgs.size === 0"
            @click="openWizard('fragments')"
          >
            带 {{ pickedMsgs.size }} 个片段开新会话
            (≈{{ fmtTokens(estimateTokens(pickedText)) }} tok)
          </button>
        </div>
        <div class="msgs">
          <div v-if="detailLoading" class="empty">加载中…</div>
          <div
            v-for="m in detailMsgs"
            :key="m.index"
            class="msg"
            :class="[m.role, { sel: pickedMsgs.has(m.index) }]"
            @click="togglePick(m.index)"
          >
            <div class="msg-head">
              <span>{{ m.role === "user" ? "用户" : "Claude" }}</span>
              <span class="msg-ts">{{ fmtTime(m.ts) }}</span>
            </div>
            <pre class="msg-text">{{ m.text }}</pre>
          </div>
        </div>
      </div>
    </div>

    <!-- 新建会话向导 -->
    <div v-if="wizardOpen" class="drawer-mask" @click.self="wizardOpen = false">
      <div class="wizard">
        <div class="drawer-head">
          <div class="drawer-title">带入上下文，开新会话</div>
          <button class="btn ghost" @click="wizardOpen = false">取消</button>
        </div>

        <div class="wz-sessions">
          <span v-for="s in selectedSessions" :key="s.session_id" class="chip">
            {{ s.title.slice(0, 24) }} · {{ fmtTokens(s.context_tokens) }} tok
          </span>
        </div>

        <div class="wz-modes">
          <label
            class="mode"
            :class="{ on: wizardMode === 'fork', off: selectedSessions.length !== 1 }"
          >
            <input
              type="radio"
              value="fork"
              v-model="wizardMode"
              :disabled="selectedSessions.length !== 1"
            />
            <div>
              <div class="mode-name">完整分叉</div>
              <div class="mode-desc">
                原生 --fork-session，无损带全部历史。起步成本
                <b :class="tokenClass(forkCost)">≈ {{ fmtTokens(forkCost) }} tok</b>
                <template v-if="selectedSessions.length !== 1">（仅支持单个会话）</template>
              </div>
            </div>
          </label>
          <label class="mode" :class="{ on: wizardMode === 'digest' }">
            <input type="radio" value="digest" v-model="wizardMode" />
            <div>
              <div class="mode-name">摘要蒸馏（省 token）</div>
              <div class="mode-desc">
                后台调用 claude -p 把选中会话蒸馏成交接摘要，可编辑后注入新会话
              </div>
            </div>
          </label>
          <label class="mode" :class="{ on: wizardMode === 'manual' }">
            <input type="radio" value="manual" v-model="wizardMode" />
            <div>
              <div class="mode-name">手写 / 片段</div>
              <div class="mode-desc">直接编辑要注入的背景文本</div>
            </div>
          </label>
        </div>

        <template v-if="wizardMode !== 'fork'">
          <div class="wz-digest-bar">
            <button
              v-if="wizardMode === 'digest'"
              class="btn primary sm"
              :disabled="digesting"
              @click="runDigest"
            >
              {{ digesting ? `蒸馏中… ${digestElapsed}s` : digestText ? "重新蒸馏" : "生成摘要" }}
            </button>
            <span class="hint"
              >注入成本 ≈
              <b :class="tokenClass(digestCost)">{{ fmtTokens(digestCost) }} tok</b>
              （完整分叉为 {{ fmtTokens(forkCost) }}）</span
            >
          </div>
          <textarea
            v-model="digestText"
            class="digest"
            :placeholder="
              wizardMode === 'digest'
                ? '点击「生成摘要」，或直接粘贴/编辑…'
                : '写下要带入新会话的背景上下文…'
            "
          ></textarea>
        </template>

        <div class="wz-foot">
          <div class="tgt">
            <button
              class="tgt-btn"
              :class="{ on: target === 'desktop' }"
              @click="setTarget('desktop')"
            >
              Desktop
            </button>
            <button
              class="tgt-btn"
              :class="{ on: target === 'terminal' }"
              @click="setTarget('terminal')"
            >
              Terminal
            </button>
          </div>
          <input v-model="wizardCwd" class="cwd-input" placeholder="新会话工作目录" />
          <button class="btn primary" :disabled="!canLaunch || launching" @click="launch">
            {{
              launching
                ? "启动中…"
                : target === "desktop"
                  ? "在 Desktop 启动新会话"
                  : "在 Terminal 启动新会话"
            }}
          </button>
        </div>
        <div
          v-if="target === 'desktop' && wizardMode !== 'fork'"
          class="hint pad"
        >
          Desktop 模式：新会话输入框会预填带入内容（上限约 1.4 万字符，当前
          {{ digestText.length }}）；片段特别长时建议用 Terminal。
        </div>
        <div v-if="target === 'desktop' && wizardMode === 'fork'" class="hint pad">
          Desktop 模式：会把原会话复制成新 ID 后导入 Desktop，原会话不受影响。
        </div>
        <div v-if="wizardErr" class="error">{{ wizardErr }}</div>
      </div>
    </div>
  </div>
</template>

<style>
* {
  margin: 0;
  padding: 0;
  box-sizing: border-box;
}
:root {
  --bg: #0f1117;
  --bg2: #161a23;
  --bg3: #1e2430;
  --border: #2a3040;
  --text: #d8dee9;
  --dim: #7b8496;
  --accent: #7aa2f7;
  --green: #9ece6a;
  --yellow: #e0af68;
  --orange: #ff9e64;
  --red: #f7768e;
}
html,
body,
#app {
  height: 100%;
  background: var(--bg);
  color: var(--text);
  font-family: -apple-system, "PingFang SC", "Helvetica Neue", sans-serif;
  font-size: 13px;
}
.app {
  display: flex;
  height: 100vh;
  overflow: hidden;
}
.sidebar {
  width: 220px;
  min-width: 220px;
  background: var(--bg2);
  border-right: 1px solid var(--border);
  padding: 14px 10px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
}
.brand {
  font-weight: 700;
  font-size: 14px;
  padding: 4px 8px 14px;
  letter-spacing: 0.3px;
}
.brand-dot {
  display: inline-block;
  width: 9px;
  height: 9px;
  border-radius: 50%;
  background: var(--accent);
  margin-right: 6px;
}
.proj {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 7px 10px;
  border-radius: 8px;
  cursor: pointer;
  color: var(--dim);
  margin-bottom: 2px;
}
.proj:hover {
  background: var(--bg3);
  color: var(--text);
}
.proj.active {
  background: var(--bg3);
  color: var(--accent);
  font-weight: 600;
}
.proj-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.proj-count {
  font-size: 11px;
  background: var(--bg);
  border-radius: 10px;
  padding: 1px 7px;
}
.sidebar-foot {
  margin-top: auto;
  padding: 10px 8px 2px;
  color: var(--dim);
  font-size: 11px;
}
.main {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.toolbar {
  display: flex;
  gap: 8px;
  padding: 12px 16px;
  border-bottom: 1px solid var(--border);
  background: var(--bg2);
}
.search {
  flex: 1;
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 8px 12px;
  color: var(--text);
  outline: none;
}
.search:focus {
  border-color: var(--accent);
}
.sel,
.cwd-input {
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 8px 10px;
  color: var(--text);
  outline: none;
}
.btn {
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 8px 14px;
  cursor: pointer;
  background: var(--bg3);
  color: var(--text);
  font-size: 13px;
}
.btn.sm {
  padding: 5px 10px;
  font-size: 12px;
}
.btn.primary {
  background: var(--accent);
  border-color: var(--accent);
  color: #0f1117;
  font-weight: 600;
}
.btn.ghost {
  background: transparent;
}
.btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
.btn:not(:disabled):hover {
  filter: brightness(1.15);
}
.list {
  flex: 1;
  overflow-y: auto;
  padding: 10px 16px 30px;
}
.card {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  background: var(--bg2);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 11px 13px;
  margin-bottom: 8px;
  cursor: pointer;
  transition: border-color 0.12s;
}
.card:hover {
  border-color: var(--accent);
}
.card.picked {
  border-color: var(--accent);
  background: #1a2233;
}
.cb {
  margin-top: 4px;
  accent-color: var(--accent);
  width: 15px;
  height: 15px;
  cursor: pointer;
}
.card-body {
  flex: 1;
  min-width: 0;
}
.card-title {
  font-weight: 600;
  margin-bottom: 6px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.star {
  color: var(--yellow);
  margin-right: 3px;
}
.card-meta {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  color: var(--dim);
  font-size: 11.5px;
  align-items: center;
}
.badge {
  padding: 1px 8px;
  border-radius: 9px;
  font-weight: 700;
  font-size: 11px;
}
.tok-green {
  color: var(--green);
  background: rgba(158, 206, 106, 0.12);
}
.tok-yellow {
  color: var(--yellow);
  background: rgba(224, 175, 104, 0.12);
}
.tok-orange {
  color: var(--orange);
  background: rgba(255, 158, 100, 0.14);
}
.tok-red {
  color: var(--red);
  background: rgba(247, 118, 142, 0.14);
}
b.tok-green,
b.tok-yellow,
b.tok-orange,
b.tok-red {
  background: none;
}
.branch,
.model {
  color: var(--dim);
}
.cwd {
  color: #556070;
}
.empty {
  text-align: center;
  color: var(--dim);
  padding: 40px;
}
.error {
  color: var(--red);
  padding: 8px 16px;
  font-size: 12px;
  word-break: break-all;
}
.drawer-mask {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.55);
  display: flex;
  justify-content: flex-end;
  z-index: 50;
}
.drawer {
  width: min(760px, 92vw);
  background: var(--bg2);
  border-left: 1px solid var(--border);
  display: flex;
  flex-direction: column;
}
.drawer-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  padding: 16px;
  border-bottom: 1px solid var(--border);
  gap: 10px;
}
.drawer-title {
  font-size: 15px;
  font-weight: 700;
}
.drawer-sub {
  color: var(--dim);
  font-size: 11px;
  margin-top: 4px;
  word-break: break-all;
}
.drawer-tools {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 10px 16px;
  border-bottom: 1px solid var(--border);
  gap: 10px;
}
.hint {
  color: var(--dim);
  font-size: 12px;
}
.msgs {
  flex: 1;
  overflow-y: auto;
  padding: 14px 16px;
}
.msg {
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 10px 12px;
  margin-bottom: 10px;
  cursor: pointer;
}
.msg.user {
  background: #182031;
}
.msg.assistant {
  background: var(--bg);
}
.msg.sel {
  border-color: var(--accent);
  box-shadow: 0 0 0 1px var(--accent);
}
.msg-head {
  display: flex;
  justify-content: space-between;
  font-size: 11px;
  color: var(--dim);
  margin-bottom: 6px;
  font-weight: 700;
}
.msg.user .msg-head span:first-child {
  color: var(--accent);
}
.msg.assistant .msg-head span:first-child {
  color: var(--green);
}
.msg-text {
  white-space: pre-wrap;
  word-break: break-word;
  font-family: inherit;
  font-size: 12.5px;
  line-height: 1.55;
  max-height: 300px;
  overflow-y: auto;
}
.wizard {
  width: min(820px, 94vw);
  background: var(--bg2);
  border-left: 1px solid var(--border);
  display: flex;
  flex-direction: column;
  padding-bottom: 14px;
}
.wz-sessions {
  padding: 12px 16px 0;
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.chip {
  background: var(--bg3);
  border: 1px solid var(--border);
  border-radius: 14px;
  padding: 3px 11px;
  font-size: 11.5px;
  color: var(--dim);
}
.wz-modes {
  padding: 12px 16px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.mode {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 10px 12px;
  cursor: pointer;
}
.mode.on {
  border-color: var(--accent);
  background: #1a2233;
}
.mode.off {
  opacity: 0.45;
}
.mode input {
  margin-top: 3px;
  accent-color: var(--accent);
}
.mode-name {
  font-weight: 700;
  margin-bottom: 3px;
}
.mode-desc {
  color: var(--dim);
  font-size: 12px;
  line-height: 1.5;
}
.wz-digest-bar {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 0 16px 8px;
}
.digest {
  margin: 0 16px;
  flex: 1;
  min-height: 220px;
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 12px;
  color: var(--text);
  font-size: 12.5px;
  line-height: 1.6;
  resize: none;
  outline: none;
  font-family: inherit;
}
.digest:focus {
  border-color: var(--accent);
}
.wz-foot {
  display: flex;
  gap: 10px;
  padding: 12px 16px 0;
  align-items: center;
}
.cwd-input {
  flex: 1;
  font-size: 12px;
}
.tgt {
  display: flex;
  border: 1px solid var(--border);
  border-radius: 8px;
  overflow: hidden;
}
.tgt-btn {
  background: var(--bg);
  color: var(--dim);
  border: none;
  padding: 8px 12px;
  cursor: pointer;
  font-size: 12px;
}
.tgt-btn.on {
  background: var(--accent);
  color: #0f1117;
  font-weight: 700;
}
.hint.pad {
  padding: 8px 16px 0;
  display: block;
}
.desk-btn {
  align-self: center;
  white-space: nowrap;
  color: var(--dim);
  border-color: transparent;
}
.card:hover .desk-btn {
  color: var(--accent);
  border-color: var(--border);
}
</style>
