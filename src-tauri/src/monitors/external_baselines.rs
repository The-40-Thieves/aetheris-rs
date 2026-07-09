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
    let dora_local = compute_git_dora().unwrap_or_else(dora_unavailable);

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
