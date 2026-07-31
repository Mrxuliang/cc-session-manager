use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Clone, Default)]
pub struct SessionMeta {
    pub session_id: String,
    pub path: String,
    pub project_slug: String,
    pub cwd: String,
    pub git_branch: String,
    pub title: String,
    pub custom_title: bool,
    pub first_ts: String,
    pub last_ts: String,
    pub user_msgs: u32,
    pub assistant_msgs: u32,
    pub file_size: u64,
    /// 当前上下文规模 ≈ 最后一条 assistant 的 input + cache_read + cache_creation
    pub context_tokens: u64,
    /// 整个会话累计输出 token
    pub total_output_tokens: u64,
    pub model: String,
}

#[derive(Serialize, Clone)]
pub struct MsgView {
    pub index: u32,
    pub role: String,
    pub text: String,
    pub ts: String,
}

fn cache() -> &'static Mutex<HashMap<String, (u64, u64, SessionMeta)>> {
    static C: OnceLock<Mutex<HashMap<String, (u64, u64, SessionMeta)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}

fn json_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}

/// 从 message.content 提取纯文本；tool_use 以标记形式保留
fn extract_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut parts: Vec<String> = Vec::new();
            let mut tools: Vec<String> = Vec::new();
            for b in blocks {
                match json_str(b, "type") {
                    "text" => {
                        let t = json_str(b, "text");
                        if !t.is_empty() {
                            parts.push(t.to_string());
                        }
                    }
                    "tool_use" => tools.push(json_str(b, "name").to_string()),
                    _ => {}
                }
            }
            if parts.is_empty() && !tools.is_empty() {
                format!("[调用工具: {}]", tools.join(", "))
            } else {
                if !tools.is_empty() {
                    parts.push(format!("[调用工具: {}]", tools.join(", ")));
                }
                parts.join("\n")
            }
        }
        _ => String::new(),
    }
}

fn is_noise_user_text(t: &str) -> bool {
    let t = t.trim_start();
    t.is_empty()
        || t.starts_with('<')
        || t.starts_with("Caveat:")
        || t.starts_with("[Request interrupted")
}

fn parse_session_file(path: &Path, project_slug: &str) -> Option<SessionMeta> {
    let f = fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let reader = BufReader::with_capacity(1 << 20, f);

    let mut m = SessionMeta {
        session_id: path.file_stem()?.to_string_lossy().to_string(),
        path: path.to_string_lossy().to_string(),
        project_slug: project_slug.to_string(),
        file_size: size,
        ..Default::default()
    };
    let mut first_user_text = String::new();
    let mut summary_title = String::new();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let ts = json_str(&v, "timestamp");
        if !ts.is_empty() {
            if m.first_ts.is_empty() {
                m.first_ts = ts.to_string();
            }
            m.last_ts = ts.to_string();
        }
        if m.cwd.is_empty() {
            m.cwd = json_str(&v, "cwd").to_string();
        }
        let gb = json_str(&v, "gitBranch");
        if !gb.is_empty() {
            m.git_branch = gb.to_string();
        }
        let sidechain = v
            .get("isSidechain")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        match json_str(&v, "type") {
            "user" if !sidechain => {
                if v.get("isMeta").and_then(|x| x.as_bool()).unwrap_or(false) {
                    continue;
                }
                if let Some(msg) = v.get("message") {
                    let text = extract_text(msg.get("content").unwrap_or(&Value::Null));
                    if !is_noise_user_text(&text) {
                        m.user_msgs += 1;
                        if first_user_text.is_empty() {
                            first_user_text = text.chars().take(120).collect();
                        }
                    }
                }
            }
            "assistant" if !sidechain => {
                m.assistant_msgs += 1;
                if let Some(msg) = v.get("message") {
                    let model = json_str(msg, "model");
                    if !model.is_empty() && model != "<synthetic>" {
                        m.model = model.to_string();
                    }
                    if let Some(u) = msg.get("usage") {
                        let g = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                        let ctx = g("input_tokens")
                            + g("cache_read_input_tokens")
                            + g("cache_creation_input_tokens");
                        if ctx > 0 {
                            m.context_tokens = ctx;
                        }
                        m.total_output_tokens += g("output_tokens");
                    }
                }
            }
            "custom-title" => {
                let t = json_str(&v, "customTitle");
                if !t.is_empty() {
                    m.title = t.to_string();
                    m.custom_title = true;
                }
            }
            "summary" => {
                let s = json_str(&v, "summary");
                if !s.is_empty() {
                    summary_title = s.to_string();
                }
            }
            _ => {}
        }
    }

    if m.title.is_empty() {
        m.title = if !summary_title.is_empty() {
            summary_title
        } else if !first_user_text.is_empty() {
            first_user_text.replace('\n', " ")
        } else {
            "(空会话)".to_string()
        };
    }
    if m.user_msgs == 0 && m.assistant_msgs == 0 {
        return None;
    }
    Some(m)
}

#[tauri::command]
async fn scan_sessions() -> Result<Vec<SessionMeta>, String> {
    let root = home().join(".claude/projects");
    let mut out: Vec<SessionMeta> = Vec::new();
    let projects = fs::read_dir(&root).map_err(|e| format!("无法读取 {:?}: {}", root, e))?;
    for p in projects.flatten() {
        if !p.path().is_dir() {
            continue;
        }
        let slug = p.file_name().to_string_lossy().to_string();
        let Ok(files) = fs::read_dir(p.path()) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().map(|e| e != "jsonl").unwrap_or(true) {
                continue;
            }
            let Ok(md) = f.metadata() else { continue };
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let key = path.to_string_lossy().to_string();
            {
                let c = cache().lock().unwrap();
                if let Some((cm, cs, meta)) = c.get(&key) {
                    if *cm == mtime && *cs == md.len() {
                        out.push(meta.clone());
                        continue;
                    }
                }
            }
            if let Some(meta) = parse_session_file(&path, &slug) {
                cache()
                    .lock()
                    .unwrap()
                    .insert(key, (mtime, md.len(), meta.clone()));
                out.push(meta);
            }
        }
    }
    out.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    Ok(out)
}

#[tauri::command]
async fn read_session(path: String) -> Result<Vec<MsgView>, String> {
    let f = fs::File::open(&path).map_err(|e| e.to_string())?;
    let reader = BufReader::with_capacity(1 << 20, f);
    let mut out = Vec::new();
    let mut idx: u32 = 0;
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("isSidechain")
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        if v.get("isMeta").and_then(|x| x.as_bool()).unwrap_or(false) {
            continue;
        }
        let role = json_str(&v, "type").to_string();
        if role != "user" && role != "assistant" {
            continue;
        }
        let Some(msg) = v.get("message") else { continue };
        let mut text = extract_text(msg.get("content").unwrap_or(&Value::Null));
        if role == "user" && is_noise_user_text(&text) {
            continue;
        }
        if text.trim().is_empty() {
            continue;
        }
        if text.chars().count() > 20000 {
            text = text.chars().take(20000).collect::<String>() + "\n…[已截断]";
        }
        out.push(MsgView {
            index: idx,
            role,
            text,
            ts: json_str(&v, "timestamp").to_string(),
        });
        idx += 1;
    }
    Ok(out)
}

/// 把若干会话拼成一份供蒸馏的转写文本
fn build_transcript(sessions: &[(String, String)]) -> Result<String, String> {
    const PER_MSG: usize = 4000;
    const TOTAL: usize = 400_000;
    let mut all = String::new();
    for (title, path) in sessions {
        all.push_str(&format!("\n\n===== 会话「{}」 =====\n", title));
        let f = fs::File::open(path).map_err(|e| e.to_string())?;
        let reader = BufReader::with_capacity(1 << 20, f);
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v.get("isSidechain")
                .and_then(|x| x.as_bool())
                .unwrap_or(false)
                || v.get("isMeta").and_then(|x| x.as_bool()).unwrap_or(false)
            {
                continue;
            }
            let role = json_str(&v, "type");
            if role != "user" && role != "assistant" {
                continue;
            }
            let Some(msg) = v.get("message") else { continue };
            let mut text = extract_text(msg.get("content").unwrap_or(&Value::Null));
            if role == "user" && is_noise_user_text(&text) {
                continue;
            }
            if text.trim().is_empty() {
                continue;
            }
            if text.chars().count() > PER_MSG {
                text = text.chars().take(PER_MSG).collect::<String>() + "…[截断]";
            }
            all.push_str(if role == "user" {
                "\n[用户]\n"
            } else {
                "\n[助手]\n"
            });
            all.push_str(&text);
            all.push('\n');
        }
    }
    // 超长时保留开头 1/8 + 结尾 7/8（近期内容更重要）
    let chars: Vec<char> = all.chars().collect();
    if chars.len() > TOTAL {
        let head: String = chars[..TOTAL / 8].iter().collect();
        let tail: String = chars[chars.len() - TOTAL * 7 / 8..].iter().collect();
        all = format!("{}\n\n……[中间部分因过长已省略]……\n\n{}", head, tail);
    }
    Ok(all)
}

#[tauri::command]
async fn generate_digest(sessions: Vec<(String, String)>) -> Result<String, String> {
    let transcript = build_transcript(&sessions)?;
    let prompt = "你会在标准输入收到一份或多份 Claude Code 历史会话的转写记录。请把它们蒸馏成一份交接摘要，作为新会话的背景上下文使用。要求：中文、Markdown 格式，依次包含：1) 任务背景与目标；2) 已完成的关键工作与重要决策（保留关键文件路径、命令、配置、服务器/端口等具体信息）；3) 踩过的坑与已验证的结论；4) 未完成事项与建议的下一步。只输出摘要本身，不要任何额外说明。";
    let mut child = Command::new("/bin/zsh")
        .args([
            "-lc",
            &format!("claude -p --no-session-persistence {}", shell_quote(prompt)),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 claude 失败: {}", e))?;
    {
        let mut stdin = child.stdin.take().ok_or("无法写入 stdin")?;
        stdin
            .write_all(transcript.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "claude -p 失败: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return Err("claude 返回了空摘要".into());
    }
    Ok(s)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn uuid_v4() -> Result<String, String> {
    use std::io::Read;
    let mut buf = [0u8; 16];
    let mut f = fs::File::open("/dev/urandom").map_err(|e| e.to_string())?;
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    buf[6] = (buf[6] & 0x0f) | 0x40;
    buf[8] = (buf[8] & 0x3f) | 0x80;
    let h: Vec<String> = buf.iter().map(|b| format!("{:02x}", b)).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        h[0..4].join(""),
        h[4..6].join(""),
        h[6..8].join(""),
        h[8..10].join(""),
        h[10..16].join("")
    ))
}

fn open_url(url: &str) -> Result<(), String> {
    let st = Command::new("open")
        .arg(url)
        .status()
        .map_err(|e| e.to_string())?;
    if !st.success() {
        return Err(format!("打开 {} 失败", url));
    }
    Ok(())
}

/// 直接把某个会话导入 Claude Desktop 打开（继续原会话）
#[tauri::command]
async fn open_in_desktop(session_id: String) -> Result<(), String> {
    open_url(&format!("claude://resume?session={}", session_id))
}

/// 复制 jsonl 为新 UUID（改写各行 sessionId，原文件不动），再让 Desktop 导入 —— 等效分叉
#[tauri::command]
async fn fork_in_desktop(path: String) -> Result<String, String> {
    let src = PathBuf::from(&path);
    let dir = src.parent().ok_or("路径无父目录")?;
    let new_id = uuid_v4()?;
    let dst = dir.join(format!("{}.jsonl", new_id));
    let f = fs::File::open(&src).map_err(|e| e.to_string())?;
    let reader = BufReader::with_capacity(1 << 20, f);
    let mut out = String::new();
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.is_empty() {
            continue;
        }
        if let Ok(mut v) = serde_json::from_str::<Value>(&line) {
            if v.get("sessionId").is_some() {
                v["sessionId"] = Value::String(new_id.clone());
            }
            out.push_str(&v.to_string());
        } else {
            out.push_str(&line);
        }
        out.push('\n');
    }
    fs::write(&dst, out).map_err(|e| e.to_string())?;
    open_url(&format!("claude://resume?session={}", new_id))?;
    Ok(new_id)
}

/// 在 Desktop 新建会话，预填提示词与工作目录（q 上限 ~14336 字符，超出会被 Desktop 截断）
#[tauri::command]
async fn new_desktop_session(cwd: String, prompt: String) -> Result<(), String> {
    let mut url = format!("claude://code/new?q={}", percent_encode(&prompt));
    if !cwd.is_empty() {
        url.push_str(&format!("&folder={}", percent_encode(&cwd)));
    }
    open_url(&url)
}

fn run_in_terminal(script_body: &str) -> Result<(), String> {
    let dir = home().join(".cc-session-manager");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let script_path = dir.join(format!("launch-{}.sh", ms));
    fs::write(&script_path, script_body).map_err(|e| e.to_string())?;
    let cmd = format!("/bin/zsh {}", shell_quote(&script_path.to_string_lossy()));
    let status = Command::new("osascript")
        .args([
            "-e",
            &format!(
                "tell application \"Terminal\"\nactivate\ndo script {}\nend tell",
                serde_json::to_string(&cmd).unwrap()
            ),
        ])
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("打开 Terminal 失败".into());
    }
    Ok(())
}

/// mode: "fork"(arg=session_id) | "prompt"(arg=首条消息全文) | "plain"
#[tauri::command]
async fn launch_session(cwd: String, mode: String, arg: String) -> Result<(), String> {
    let cd = if cwd.is_empty() {
        String::new()
    } else {
        format!("cd {} 2>/dev/null\n", shell_quote(&cwd))
    };
    let body = match mode.as_str() {
        "fork" => format!(
            "#!/bin/zsh\n{}exec claude --resume {} --fork-session\n",
            cd,
            shell_quote(&arg)
        ),
        "prompt" => {
            let dir = home().join(".cc-session-manager/handoffs");
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis();
            let pf = dir.join(format!("handoff-{}.md", ms));
            fs::write(&pf, &arg).map_err(|e| e.to_string())?;
            format!(
                "#!/bin/zsh\n{}exec claude \"$(cat {})\"\n",
                cd,
                shell_quote(&pf.to_string_lossy())
            )
        }
        _ => format!("#!/bin/zsh\n{}exec claude\n", cd),
    };
    run_in_terminal(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_real_projects_dir() {
        let root = home().join(".claude/projects");
        if !root.exists() {
            return;
        }
        let mut n = 0;
        for p in fs::read_dir(&root).unwrap().flatten() {
            if !p.path().is_dir() {
                continue;
            }
            let slug = p.file_name().to_string_lossy().to_string();
            for f in fs::read_dir(p.path()).unwrap().flatten() {
                let path = f.path();
                if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                    if let Some(m) = parse_session_file(&path, &slug) {
                        assert!(!m.title.is_empty());
                        n += 1;
                        if n <= 5 {
                            println!(
                                "OK: {} | ctx={} tok | {}问/{}答 | {}",
                                m.title.chars().take(30).collect::<String>(),
                                m.context_tokens,
                                m.user_msgs,
                                m.assistant_msgs,
                                m.cwd
                            );
                        }
                    }
                }
            }
        }
        println!("parsed {} sessions", n);
        assert!(n > 0, "应至少解析出一个会话");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            scan_sessions,
            read_session,
            generate_digest,
            launch_session,
            open_in_desktop,
            fork_in_desktop,
            new_desktop_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
