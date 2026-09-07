// Handing the comments to an agent: `s` (in READ and in the comments list).
//
// akapen's model, kept as is: `y` copies and keeps, `s` delivers and — on
// success only — clears the slate. Where the text goes:
//
// - `--send-cmd <cmd>`: piped to the command's stdin (`sh -c`), for any
//   destination (`cat >> review.txt`, a fixed pane, another tool).
// - Otherwise, inside a herdr pane (`HERDR_PANE_ID` set): the sole agent
//   in this tab, else the sole agent in the workspace, via
//   `herdr agent prompt <pane> <text>`. The text is one argv element, so
//   quotes and `$` in a comment can never break the delivery. The rule and
//   the `herdr agent list` reading are akapen's (`export.rs`).
// - Neither: `s` says so and points at `y`.
//
// A failed delivery keeps every comment for a retry; the clipboard copy
// that `s` also makes has already happened either way.

use super::*;

/// Where `s` delivers, decided once at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SendTarget {
    /// `--send-cmd`: the shell command the export is piped to.
    Command(String),
    /// Inside herdr: resolve the agent pane at send time.
    HerdrAgent,
    /// Nowhere; `y` is what there is.
    None,
}

impl SendTarget {
    /// `--send-cmd` wins; else herdr when its pane variables are in the
    /// environment; else nothing.
    pub(crate) fn detect(send_cmd: Option<String>, env: &dyn Fn(&str) -> Option<String>) -> Self {
        match send_cmd {
            Some(cmd) if !cmd.trim().is_empty() => SendTarget::Command(cmd),
            _ if env("HERDR_PANE_ID").is_some_and(|v| !v.is_empty()) => SendTarget::HerdrAgent,
            _ => SendTarget::None,
        }
    }
}

/// `s`: copy every comment to the clipboard, deliver it to the send
/// target, and clear the comments when the delivery succeeded.
pub(crate) fn send_comments(app: &mut App, ctx: &Ctx) {
    if app.comments.is_empty() {
        app.toast(t!("送るコメントがありません", "no comments to send"));
        return;
    }
    let text = format_all(&app.comments);
    let count = app.comments.len();
    let copied = copy_to_clipboard(&text);
    let delivered: Result<String, String> = match &ctx.send_target {
        SendTarget::Command(cmd) => send_command(cmd, &text).map(|()| cmd.clone()),
        SendTarget::HerdrAgent => {
            resolve_agent_pane().and_then(|pane| send_to_agent(&pane, &text).map(|()| pane))
        }
        SendTarget::None => Err(t!(
            "送り先がありません（herdr の外では --send-cmd で指定）",
            "nowhere to send (outside herdr, name a --send-cmd)"
        )),
    };
    match delivered {
        Ok(target) => {
            app.comments.clear();
            app.laid_width = 0; // the cards go with them
            app.note(t!(
                "✓ コメント {count} 件を {target} へ送りました",
                "✓ sent {count} comment(s) to {target}"
            ));
        }
        Err(e) => {
            let kept = if copied {
                t!(
                    "コメントは残しています（クリップボードにはコピー済み）",
                    "comments kept (copied to the clipboard)"
                )
            } else {
                t!("コメントは残しています", "comments kept")
            };
            app.toast_err(format!("{e} · {kept}"));
        }
    }
}

/// Pipe `text` to `sh -c <cmd>`.
fn send_command(cmd: &str, text: &str) -> Result<(), String> {
    let mut child = Command::new("sh")
        .args(["-c", cmd])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            t!(
                "送信コマンドを起動できません: {e}",
                "cannot start the send command: {e}"
            )
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(t!(
            "送信コマンドが失敗しました: {err}",
            "the send command failed: {err}"
        ))
    }
}

/// The pane id of the sole herdr agent in this tab, else in this
/// workspace. Anything else is a refusal with the reason the toast shows.
fn resolve_agent_pane() -> Result<String, String> {
    let out = Command::new("herdr")
        .args(["agent", "list"])
        .output()
        .map_err(|e| t!("herdr を起動できません: {e}", "cannot run herdr: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(t!(
            "herdr agent list が失敗しました: {err}",
            "herdr agent list failed: {err}"
        ));
    }
    let agents = parse_agents(&String::from_utf8_lossy(&out.stdout))?;
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    pick_agent(
        &agents,
        env("HERDR_TAB_ID").as_deref(),
        env("HERDR_WORKSPACE_ID").as_deref(),
        env("HERDR_PANE_ID").as_deref(),
    )
}

/// The agents array from `herdr agent list`: a bare array, `result.agents`,
/// or `agents` (the envelope is not pinned; akapen accepts all three).
fn parse_agents(json: &str) -> Result<Vec<serde_json::Value>, String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| {
        t!(
            "エージェント一覧を読めません: {e}",
            "cannot read the agent list: {e}"
        )
    })?;
    if let Some(array) = value.as_array() {
        return Ok(array.clone());
    }
    value
        .get("result")
        .and_then(|r| r.get("agents"))
        .or_else(|| value.get("agents"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .ok_or_else(|| {
            t!(
                "エージェント一覧に agents がありません",
                "the agent list has no agents array"
            )
        })
}

/// The sole agent in `tab`, else the sole agent in `ws`, never our own
/// pane (`me`).
pub(crate) fn pick_agent(
    agents: &[serde_json::Value],
    tab: Option<&str>,
    ws: Option<&str>,
    me: Option<&str>,
) -> Result<String, String> {
    let in_tab = candidates(agents, "tab_id", tab, me);
    if let [agent] = in_tab.as_slice() {
        return pane_id(agent).ok_or_else(|| {
            t!(
                "エージェントに pane_id がありません",
                "agent entry has no pane_id"
            )
        });
    }
    match candidates(agents, "workspace_id", ws, me).as_slice() {
        [agent] => pane_id(agent).ok_or_else(|| {
            t!(
                "エージェントに pane_id がありません",
                "agent entry has no pane_id"
            )
        }),
        [] if in_tab.is_empty() => Err(t!(
            "このタブにもワークスペースにもエージェントがいません",
            "no agent in this tab or workspace"
        )),
        _ => Err(t!(
            "エージェントが複数います — --send-cmd で1つを名指しして",
            "several agents here — name one with --send-cmd"
        )),
    }
}

/// The real agents whose `key` equals `want`, ignoring our own pane: only
/// entries carrying an `agent` field count (`herdr agent list` returns
/// every pane; plugin sidebars and plain shells have no `agent` field).
fn candidates<'a>(
    agents: &'a [serde_json::Value],
    key: &str,
    want: Option<&str>,
    me: Option<&str>,
) -> Vec<&'a serde_json::Value> {
    let Some(want) = want else { return Vec::new() };
    agents
        .iter()
        .filter(|a| a.get("agent").and_then(serde_json::Value::as_str).is_some())
        .filter(|a| a.get(key).and_then(serde_json::Value::as_str) == Some(want))
        .filter(|a| pane_id(a).as_deref() != me)
        .collect()
}

fn pane_id(agent: &serde_json::Value) -> Option<String> {
    agent
        .get("pane_id")
        .and_then(serde_json::Value::as_str)
        .map(String::from)
}

/// `herdr agent prompt <pane> <text>`: the text is one argument, so
/// nothing in it is ever interpreted by a shell.
fn send_to_agent(pane: &str, text: &str) -> Result<(), String> {
    let out = Command::new("herdr")
        .args(["agent", "prompt", pane, text])
        .output()
        .map_err(|e| t!("herdr を起動できません: {e}", "cannot run herdr: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(t!(
            "herdr agent prompt が失敗しました: {err}",
            "herdr agent prompt failed: {err}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// タブに1つならそれ、無ければワークスペースの1つ、自分のペインは数えない、
    /// 複数なら断る。`agent` フィールドの無いペイン(素のシェル)は候補でない。
    #[test]
    fn the_sole_agent_in_the_tab_wins_then_the_workspace_then_a_refusal() {
        let agents = vec![
            json!({"pane_id": "p1", "tab_id": "t1", "workspace_id": "w", "agent": "claude"}),
            json!({"pane_id": "p2", "tab_id": "t2", "workspace_id": "w", "agent": "codex"}),
            json!({"pane_id": "p3", "tab_id": "t1", "workspace_id": "w"}), // a plain shell
            json!({"pane_id": "me", "tab_id": "t1", "workspace_id": "w", "agent": "claude"}),
        ];
        assert_eq!(
            pick_agent(&agents, Some("t1"), Some("w"), Some("me")),
            Ok("p1".into())
        );
        assert_eq!(
            pick_agent(&agents, Some("t9"), Some("w"), Some("me")).ok(),
            None,
            "two in the workspace"
        );
        let one = &agents[1..2];
        assert_eq!(
            pick_agent(one, Some("t9"), Some("w"), None),
            Ok("p2".into()),
            "falls back to the workspace"
        );
        assert!(
            pick_agent(one, Some("t9"), Some("x"), None).is_err(),
            "no agent anywhere"
        );
    }

    #[test]
    fn the_agent_list_envelope_is_not_pinned() {
        assert_eq!(parse_agents(r#"[{"pane_id":"p"}]"#).unwrap().len(), 1);
        assert_eq!(
            parse_agents(r#"{"result":{"agents":[{"pane_id":"p"}]}}"#)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(parse_agents(r#"{"agents":[]}"#).unwrap().len(), 0);
        assert!(parse_agents(r#"{"nope":1}"#).is_err());
    }

    /// `--send-cmd` が最優先、無ければ herdr の中かどうか、どちらでもなければ無し。
    #[test]
    fn the_send_target_is_the_flag_then_herdr_then_nothing() {
        let herdr = |k: &str| (k == "HERDR_PANE_ID").then(|| "w:p1".to_string());
        let bare = |_: &str| None;
        assert_eq!(
            SendTarget::detect(Some("cat".into()), &herdr),
            SendTarget::Command("cat".into())
        );
        assert_eq!(SendTarget::detect(None, &herdr), SendTarget::HerdrAgent);
        assert_eq!(
            SendTarget::detect(Some("  ".into()), &bare),
            SendTarget::None
        );
    }
}
