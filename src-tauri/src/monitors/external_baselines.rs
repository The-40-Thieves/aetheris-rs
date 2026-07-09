//! External baselines: live LLM leaderboard ranks + git-derived DORA metrics,
//! alongside genuinely-static reference data (chip Tjmax specs, SaaS pricing).
//!
//! Previously every value here was hardcoded and commented "Simulated" while the
//! UI implied live computation. Now:
//!   * LLM ranks are fetched live from the Arena community mirror
//!     (api.wulong.dev) on a background task and cached; on fetch failure the
//!     section reports "unavailable" instead of stale/fake ranks.
//!   * DORA "local metrics" are computed from real `git log` history (deploy
//!     frequency + lead-time proxy); change-failure-rate / MTTR are marked
//!     "n/a" because they require CI / incident data not available here — we do
//!     not invent them.
//!   * Chip Tjmax limits and observability SaaS pricing are legitimately static
//!     reference data and are now labelled as such (with `as_of` + `source`),
//!     not presented as live.

use serde_json::{json, Value};
use std::process::Command;
use std::sync::Arc;
use std::sync::RwLock;
use crate::database::Database;

/// Cached live-fetched sections, refreshed by a background task.
struct Cache {
    ai_evals: Value,
    dora_local: Value,
}

static CACHE: RwLock<Option<Cache>> = RwLock::new(None);

pub fn get_external_baselines(_db: &Arc<Database>) -> Value {
    // Static reference data (facts / curated, not live).
    let hardware = json!({
        "as_of": "2026-07",
        "note": "Static reference specifications (thermal limits do not change).",
        "amd_ryzen_7000": { "tjmax_c": 95.0, "source": "AMD Spec" },
        "intel_core_14th": { "tjmax_c": 100.0, "source": "Intel ARK" },
        "apple_m_series": { "tjmax_c": "Abstracted", "community_threshold_c": 105.0, "source": "Notebookcheck / iStat" },
        "nvidia_rtx_40": { "tjmax_c": 83.0, "source": "NVIDIA Spec" }
    });

    let observability_pricing = json!({
        "as_of": "2026-07",
        "note": "Curated public list-price estimates (static reference, not live).",
        "tools": [
            { "tool": "Datadog", "tier": "Enterprise", "est_monthly_cost": "$3,000+", "source": "G2 / vendor pricing" },
            { "tool": "New Relic", "tier": "Standard", "est_monthly_cost": "$1,500+", "source": "Gartner" },
            { "tool": "Dynatrace", "tier": "Enterprise", "est_monthly_cost": "$5,000+", "source": "G2" },
            { "tool": "Aetheris (Local)", "tier": "Self-hosted", "est_monthly_cost": "$0", "source": "Local execution" }
        ]
    });

    let dora_benchmarks = json!({
        "deploy_frequency": { "elite": "On demand (multiple/day)", "low": "Monthly to 6 months" },
        "lead_time": { "elite": "Under 1 hour to 1 day", "low": "1-6 months" },
        "change_failure_rate": { "elite": "0-15%", "low": "46-60%" },
        "mttr": { "elite": "Under 1 hour", "low": "1 week to 1 month" }
    });

    // Live sections from cache (or explicit unavailable state).
    let guard = CACHE.read().unwrap();
    let (ai_evals, dora_local) = match guard.as_ref() {
        Some(c) => (c.ai_evals.clone(), c.dora_local.clone()),
        None => (ai_unavailable("not fetched yet"), dora_unavailable()),
    };

    json!({
        "hardware": hardware,
        "observability_pricing": observability_pricing.get("tools").cloned().unwrap_or(Value::Array(vec![])),
        "observability_pricing_meta": { "as_of": "2026-07", "static": true },
        "dora": { "benchmarks": dora_benchmarks, "local_metrics": dora_local },
        "ai_evals": ai_evals,
    })
}

fn ai_unavailable(reason: &str) -> Value {
    json!({
        "status": "unavailable",
        "reason": reason,
        "source": "Arena (api.wulong.dev community mirror)",
        "lmsys_chat_arena": []
    })
}

fn dora_unavailable() -> Value {
    json!({
        "status": "unavailable",
        "reason": "no git repository in working directory",
        "deploy_frequency": "n/a",
        "lead_time": "n/a",
        "change_failure_rate": "n/a",
        "mttr": "n/a"
    })
}

/// Refresh the live sections. Best-effort: a failed LLM fetch leaves an explicit
/// unavailable state for that section; DORA is recomputed from local git.
pub async fn refresh(client: &reqwest::Client) {
    let ai_evals = match fetch_arena(client).await {
        Some(v) => v,
        None => ai_unavailable("fetch failed"),
    };
    let mut dora_local = compute_git_dora().unwrap_or_else(dora_unavailable);
    // Layer real CI-derived change-failure-rate + MTTR onto the git DORA when a
    // GitHub repo + token are available. Best-effort: leaves the "n/a" values
    // untouched on any failure — never fabricated.
    if let Some((cfr, mttr, runs)) = fetch_ci_dora(client).await {
        dora_local["change_failure_rate"] = json!(cfr);
        dora_local["mttr"] = json!(mttr);
        dora_local["ci_runs_90d"] = json!(runs);
        let src = dora_local["source"].as_str().unwrap_or("git").to_string();
        dora_local["source"] = json!(format!("{src} + GitHub Actions API"));
    }

    let mut guard = CACHE.write().unwrap();
    *guard = Some(Cache { ai_evals, dora_local });
}

/// Fetch the Arena text leaderboard and shape it into `ai_evals`.
async fn fetch_arena(client: &reqwest::Client) -> Option<Value> {
    let resp = client
        .get("https://api.wulong.dev/arena-ai-leaderboards/v1/leaderboard?name=text")
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    let (rows, last_updated) = parse_arena_leaderboard(&body, 8);
    if rows.is_empty() {
        return None; // never present an empty leaderboard as success
    }
    Some(json!({
        "status": "ok",
        "source": "Arena (api.wulong.dev community mirror)",
        "last_updated": last_updated,
        "lmsys_chat_arena": rows,
    }))
}

/// Pure parser: take the top N models from an Arena leaderboard JSON.
fn parse_arena_leaderboard(body: &Value, top_n: usize) -> (Vec<Value>, Option<String>) {
    let last_updated = body
        .get("meta")
        .and_then(|m| m.get("last_updated"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let models = match body.get("models").and_then(|m| m.as_array()) {
        Some(a) => a,
        None => return (Vec::new(), last_updated),
    };
    let rows = models
        .iter()
        .take(top_n)
        .filter_map(|m| {
            let model = m.get("model")?.as_str()?;
            Some(json!({
                "model": model,
                "rank": m.get("rank").and_then(|v| v.as_i64()),
                "score": m.get("score").and_then(|v| v.as_f64()),
                "vendor": m.get("vendor").and_then(|v| v.as_str()),
            }))
        })
        .collect();
    (rows, last_updated)
}

/// Compute DORA "local metrics" from `git log` in the current directory.
/// Returns None when the cwd is not a git repository.
fn compute_git_dora() -> Option<Value> {
    // Commit author timestamps (unix) over the last 30 days.
    let out = Command::new("git")
        .args(["log", "--since=30.days", "--pretty=format:%at"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let epochs: Vec<i64> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
    Some(dora_from_commit_epochs(&epochs))
}

/// Pure DORA computation from commit timestamps. Honest about what git can and
/// cannot supply: deploy frequency and a lead-time proxy are derived; change
/// failure rate and MTTR are marked n/a (they need CI / incident data).
fn dora_from_commit_epochs(epochs: &[i64]) -> Value {
    let commits = epochs.len();
    let deploy_freq = format!("{:.1}/day (commits, 30d avg)", commits as f64 / 30.0);

    // Lead-time proxy: median gap between consecutive commits, in hours.
    let lead_time = if epochs.len() >= 2 {
        let mut sorted: Vec<i64> = epochs.to_vec();
        sorted.sort_unstable();
        let mut gaps: Vec<i64> = sorted.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_unstable();
        let median = gaps[gaps.len() / 2] as f64 / 3600.0;
        format!("{median:.1} h (median commit interval)")
    } else {
        "n/a (insufficient history)".to_string()
    };

    let tier = if commits as f64 / 30.0 >= 1.0 { "Elite (by frequency)" } else { "Medium" };

    json!({
        "status": "ok",
        "source": "git log (working directory), last 30 days",
        "deploy_frequency": deploy_freq,
        "lead_time": lead_time,
        "change_failure_rate": "n/a (requires CI integration)",
        "mttr": "n/a (requires incident data)",
        "tier_rating": tier,
        "commits_30d": commits,
    })
}

// --- CI-integrated DORA (change-failure-rate + MTTR from GitHub Actions) -------

/// One completed CI run: conclusion, creation epoch, and workflow name.
struct CiRun {
    conclusion: String,
    created: i64,
    workflow: String,
}

/// Format a duration in seconds compactly ("45m", "2.3h", "1.4d").
fn format_duration(secs: i64) -> String {
    let s = secs.max(0) as f64;
    if s < 3600.0 {
        format!("{:.0}m", s / 60.0)
    } else if s < 86_400.0 {
        format!("{:.1}h", s / 3600.0)
    } else {
        format!("{:.1}d", s / 86_400.0)
    }
}

/// Compute change-failure-rate and MTTR from CI runs. CFR = failed / (success +
/// failed). MTTR = median time from a failing run to the next successful run on
/// the same workflow. Returns (cfr, mttr, completed_run_count).
fn dora_from_runs(runs: &[CiRun]) -> (String, String, usize) {
    let completed: Vec<&CiRun> = runs
        .iter()
        .filter(|r| r.conclusion == "success" || r.conclusion == "failure")
        .collect();
    let n = completed.len();
    if n == 0 {
        return ("n/a (no completed CI runs)".into(), "n/a".into(), 0);
    }
    let failures = completed.iter().filter(|r| r.conclusion == "failure").count();
    let cfr = format!("{:.0}% ({failures}/{n} runs)", failures as f64 / n as f64 * 100.0);

    // MTTR: per workflow, time from each failure to the next success.
    let mut by_wf: std::collections::HashMap<&str, Vec<&CiRun>> = std::collections::HashMap::new();
    for r in &completed {
        by_wf.entry(r.workflow.as_str()).or_default().push(r);
    }
    let mut recoveries: Vec<i64> = Vec::new();
    for (_wf, mut list) in by_wf {
        list.sort_by_key(|r| r.created);
        for (i, r) in list.iter().enumerate() {
            if r.conclusion == "failure" {
                if let Some(succ) = list[i + 1..].iter().find(|x| x.conclusion == "success") {
                    recoveries.push(succ.created - r.created);
                }
            }
        }
    }
    let mttr = if failures == 0 {
        "0 (no failures)".into()
    } else if recoveries.is_empty() {
        "n/a (not yet recovered)".into()
    } else {
        recoveries.sort_unstable();
        format_duration(recoveries[recoveries.len() / 2])
    };
    (cfr, mttr, n)
}

/// (owner, repo) parsed from `git remote get-url origin` (https or ssh form).
fn github_repo() -> Option<(String, String)> {
    let out = Command::new("git").args(["remote", "get-url", "origin"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let path = path.trim_end_matches(".git");
    let (owner, repo) = path.split_once('/')?;
    Some((owner.to_string(), repo.to_string()))
}

/// A GitHub token from `GITHUB_TOKEN`, else the `gh` CLI (dev convenience).
fn github_token() -> Option<String> {
    if let Ok(t) = std::env::var("GITHUB_TOKEN") {
        if !t.is_empty() {
            return Some(t);
        }
    }
    let out = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if out.status.success() {
        let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}

/// Fetch recent default-branch CI runs from the GitHub Actions API and derive
/// (change_failure_rate, mttr, run_count). None when no repo/token/network.
async fn fetch_ci_dora(client: &reqwest::Client) -> Option<(String, String, usize)> {
    let (owner, repo) = github_repo()?;
    let token = github_token()?;
    // All recent completed runs (across branches) — a real CI-health signal that
    // has data as soon as any workflow has run, not just after a default-branch
    // deploy. Labeled "runs" in the output, not "deploys".
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/actions/runs?per_page=100&status=completed"
    );
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "aetheris-telemetry")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    let runs: Vec<CiRun> = body["workflow_runs"]
        .as_array()?
        .iter()
        .filter_map(|r| {
            Some(CiRun {
                conclusion: r["conclusion"].as_str()?.to_string(),
                created: chrono::DateTime::parse_from_rfc3339(r["created_at"].as_str()?)
                    .ok()?
                    .timestamp(),
                workflow: r["name"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect();
    if runs.is_empty() {
        return None;
    }
    Some(dora_from_runs(&runs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arena_leaderboard_top_n() {
        let body = json!({
            "meta": { "last_updated": "2026-07-01", "fetched_at": "2026-07-08" },
            "models": [
                { "rank": 1, "model": "Claude 5 Opus", "vendor": "Anthropic", "score": 1450.0, "license": "proprietary" },
                { "rank": 2, "model": "GPT-6", "vendor": "OpenAI", "score": 1440.0 },
                { "rank": 3, "model": "Gemini 3 Ultra", "vendor": "Google", "score": 1435.0 }
            ]
        });
        let (rows, updated) = parse_arena_leaderboard(&body, 2);
        assert_eq!(updated, Some("2026-07-01".to_string()));
        assert_eq!(rows.len(), 2, "top_n honored");
        assert_eq!(rows[0]["model"], "Claude 5 Opus");
        assert_eq!(rows[0]["rank"], 1);
        assert_eq!(rows[0]["score"], 1450.0);
    }

    #[test]
    fn arena_missing_models_yields_empty_not_panic() {
        let (rows, _) = parse_arena_leaderboard(&json!({"error": "x"}), 8);
        assert!(rows.is_empty());
    }

    #[test]
    fn dora_from_commits_reports_real_and_na() {
        // 60 commits over 30 days -> 2.0/day, tier Elite.
        let now = 1_700_000_000i64;
        let epochs: Vec<i64> = (0..60).map(|i| now - i * 3600).collect();
        let d = dora_from_commit_epochs(&epochs);
        assert!(d["deploy_frequency"].as_str().unwrap().starts_with("2.0/day"));
        assert_eq!(d["change_failure_rate"], "n/a (requires CI integration)");
        assert_eq!(d["mttr"], "n/a (requires incident data)");
        assert_eq!(d["commits_30d"], 60);
    }

    #[test]
    fn dora_handles_empty_history() {
        let d = dora_from_commit_epochs(&[]);
        assert_eq!(d["commits_30d"], 0);
        assert!(d["lead_time"].as_str().unwrap().contains("n/a"));
    }

    #[test]
    fn dora_from_ci_runs_computes_cfr_and_mttr() {
        let runs = vec![
            CiRun { conclusion: "success".into(), created: 0, workflow: "CI".into() },
            CiRun { conclusion: "failure".into(), created: 100, workflow: "CI".into() },
            CiRun { conclusion: "success".into(), created: 700, workflow: "CI".into() }, // recovers 600s later
        ];
        let (cfr, mttr, n) = dora_from_runs(&runs);
        assert_eq!(n, 3);
        assert!(cfr.starts_with("33%"), "cfr={cfr}");
        assert_eq!(mttr, "10m"); // 600s median recovery
        // No failures -> 0% / "0 (no failures)".
        let ok = vec![CiRun { conclusion: "success".into(), created: 0, workflow: "CI".into() }];
        let (cfr2, mttr2, _) = dora_from_runs(&ok);
        assert!(cfr2.starts_with("0%"));
        assert_eq!(mttr2, "0 (no failures)");
        // No runs -> honest n/a, never fabricated.
        assert_eq!(dora_from_runs(&[]).2, 0);
    }

    #[tokio::test]
    #[ignore = "live: hits the GitHub Actions API for this repo"]
    async fn ci_dora_live_probe() {
        let client = reqwest::Client::new();
        let r = fetch_ci_dora(&client).await;
        eprintln!("ci_dora = {r:?}");
        let (cfr, mttr, n) = r.expect("expected >=1 completed CI run on this repo");
        eprintln!("CFR={cfr}  MTTR={mttr}  runs={n}");
        assert!(n >= 1);
        assert!(cfr.contains('%'));
    }

    #[tokio::test]
    #[ignore = "live: hits api.wulong.dev and local git"]
    async fn live_refresh_probe() {
        let client = reqwest::Client::new();
        let ai = fetch_arena(&client).await;
        eprintln!("=== arena ===\n{}", serde_json::to_string_pretty(&ai).unwrap_or_default());
        let dora = compute_git_dora();
        eprintln!("=== dora ===\n{}", serde_json::to_string_pretty(&dora).unwrap_or_default());
        // git DORA must work inside this repo (Arena is best-effort / network).
        assert!(dora.is_some(), "git dora should compute in this repo");
    }
}
