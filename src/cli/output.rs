use chrono::{Local, TimeZone};

/// 输出格式
pub enum Fmt {
    Yaml,
    Json,
}

#[derive(Clone, Copy, Debug)]
pub struct OutputOpts {
    pub json: bool,
    pub with_meta: bool,
    pub debug_source: bool,
}

impl OutputOpts {
    pub fn request_flags(self) -> (bool, bool) {
        (self.with_meta || self.debug_source, self.debug_source)
    }
}

/// 默认 YAML，--json 时输出 JSON
pub fn resolve(json: bool) -> Fmt {
    if json {
        Fmt::Json
    } else {
        Fmt::Yaml
    }
}

pub fn print_value(value: &serde_json::Value, fmt: &Fmt) -> anyhow::Result<()> {
    match fmt {
        Fmt::Json => println!("{}", serde_json::to_string_pretty(value)?),
        Fmt::Yaml => print!("{}", serde_yaml::to_string(value)?),
    }
    Ok(())
}

pub fn print_response(data: &serde_json::Value, opts: &OutputOpts) -> anyhow::Result<()> {
    print_value(data, &resolve(opts.json))
}

pub fn emit_warnings(data: &serde_json::Value) {
    for line in warning_lines(data) {
        eprintln!("[wx] 警告：{}", line);
    }
}

pub fn warning_lines(data: &serde_json::Value) -> Vec<String> {
    let mut lines = Vec::new();
    let meta = match data.get("meta") {
        Some(v) if v.is_object() => v,
        _ => return lines,
    };

    let unknown_shards: Vec<String> = meta
        .get("unknown_shards")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if !unknown_shards.is_empty() {
        lines.push(format!(
            "磁盘上发现 daemon 不认识的分片 {}，结果可能不完整；请在本机 Terminal 运行 {}",
            unknown_shards.join(", "),
            crate::config::RECOMMENDED_KEY_EXTRACT_HINT
        ));
    }

    let status = meta.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status == "possibly_stale" || status == "possibly_stale_unknown_shards" {
        let session_ts = meta.get("session_last_timestamp").and_then(|v| v.as_i64());
        let chat_ts = meta.get("chat_latest_timestamp").and_then(|v| v.as_i64());
        if let (Some(session_ts), Some(chat_ts)) = (session_ts, chat_ts) {
            let subject = data
                .get("chat")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("username").and_then(|v| v.as_str()))
                .unwrap_or("当前查询");
            lines.push(format!(
                "session.db 显示 '{}' 最新到 {}，但本次扫描只到 {}，结果可能过期或不完整。",
                subject,
                fmt_meta_ts(session_ts),
                fmt_meta_ts(chat_ts),
            ));
        }
    }

    lines
}

pub fn warning_block_text(data: &serde_json::Value) -> Option<String> {
    let lines = warning_lines(data);
    if lines.is_empty() {
        return None;
    }
    Some(
        lines
            .into_iter()
            .map(|line| format!("[wx] 警告：{}", line))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub fn warning_block_markdown(data: &serde_json::Value) -> Option<String> {
    let lines = warning_lines(data);
    if lines.is_empty() {
        return None;
    }
    let mut out = String::from("> [!WARNING]\n");
    for line in lines {
        out.push_str("> ");
        out.push_str(&line);
        out.push('\n');
    }
    Some(out)
}

fn fmt_meta_ts(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ts.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stale_meta(status: &str) -> serde_json::Value {
        json!({
            "status": status,
            // 2023-11-14/15 depending on the local timezone
            "session_last_timestamp": 1_700_000_000,
            "chat_latest_timestamp": 1_699_990_000
        })
    }

    #[test]
    fn request_flags_follow_with_meta_and_debug_source() {
        let base = OutputOpts {
            json: false,
            with_meta: false,
            debug_source: false,
        };
        assert_eq!(base.request_flags(), (false, false));
        assert_eq!(
            OutputOpts {
                with_meta: true,
                ..base
            }
            .request_flags(),
            (true, false)
        );
        // --debug-source implies with_meta on the request side
        assert_eq!(
            OutputOpts {
                debug_source: true,
                ..base
            }
            .request_flags(),
            (true, true)
        );
    }

    #[test]
    fn missing_or_clean_meta_yields_no_warnings() {
        assert!(warning_lines(&json!({"messages": []})).is_empty());
        assert!(warning_lines(&json!({"meta": "ok"})).is_empty());
        assert!(warning_lines(&json!({"meta": {"status": "ok", "unknown_shards": []}})).is_empty());
        assert!(warning_block_text(&json!({"meta": {"status": "ok"}})).is_none());
        assert!(warning_block_markdown(&json!({"meta": {"status": "ok"}})).is_none());
    }

    #[test]
    fn unknown_shards_warning_names_shards_and_recommended_fix() {
        let data = json!({
            "meta": {"status": "ok", "unknown_shards": ["message_3.db", "message_7.db"]}
        });
        let lines = warning_lines(&data);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("message_3.db, message_7.db"));
        assert!(lines[0].contains(crate::config::RECOMMENDED_KEY_EXTRACT_HINT));
    }

    #[test]
    fn unknown_shards_ignores_non_string_entries() {
        let data = json!({"meta": {"unknown_shards": [1, null, "message_2.db"]}});
        let lines = warning_lines(&data);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("message_2.db"));
        assert!(!lines[0].contains("null"));
    }

    #[test]
    fn possibly_stale_warning_uses_chat_name_and_formatted_timestamps() {
        for status in ["possibly_stale", "possibly_stale_unknown_shards"] {
            let data = json!({"chat": "张三", "meta": stale_meta(status)});
            let lines = warning_lines(&data);
            assert_eq!(lines.len(), 1, "status {status}");
            assert!(lines[0].contains("'张三'"));
            // timestamps are rendered as local dates, not raw epoch seconds
            assert!(!lines[0].contains("1700000000"));
            assert!(lines[0].contains("2023-11-1"));
        }
    }

    #[test]
    fn possibly_stale_falls_back_to_username_then_generic_subject() {
        let by_username = warning_lines(&json!({
            "username": "wxid_abc",
            "meta": stale_meta("possibly_stale")
        }));
        assert_eq!(by_username.len(), 1);
        assert!(by_username[0].contains("'wxid_abc'"));

        let generic = warning_lines(&json!({"meta": stale_meta("possibly_stale")}));
        assert_eq!(generic.len(), 1);
        assert!(generic[0].contains("'当前查询'"));
    }

    #[test]
    fn stale_warning_needs_both_timestamps_and_a_stale_status() {
        let missing_chat_ts = json!({
            "meta": {"status": "possibly_stale", "session_last_timestamp": 1_700_000_000}
        });
        assert!(warning_lines(&missing_chat_ts).is_empty());

        let windowed = json!({"meta": stale_meta("windowed")});
        assert!(warning_lines(&windowed).is_empty());
    }

    #[test]
    fn unknown_shards_and_stale_produce_two_lines_in_order() {
        let mut meta = stale_meta("possibly_stale_unknown_shards");
        meta["unknown_shards"] = json!(["message_9.db"]);
        let data = json!({"chat": "AI群", "meta": meta});

        let lines = warning_lines(&data);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("message_9.db"));
        assert!(lines[1].contains("'AI群'"));

        let block = warning_block_text(&data).unwrap();
        let block_lines: Vec<&str> = block.lines().collect();
        assert_eq!(block_lines.len(), 2);
        assert!(block_lines.iter().all(|l| l.starts_with("[wx] 警告：")));
    }

    #[test]
    fn markdown_block_is_a_github_warning_admonition() {
        let data = json!({"meta": {"unknown_shards": ["message_3.db"]}});
        let block = warning_block_markdown(&data).unwrap();
        assert!(block.starts_with("> [!WARNING]\n> "));
        assert!(block.ends_with('\n'));
        assert!(block.contains("message_3.db"));
        assert_eq!(block.lines().count(), 2);
    }
}
