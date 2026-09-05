//! Rate-limit probe for external schedulers (`--usage-json`, `--wait-until`).
//!
//! A long-running orchestrator (a Claude Code Workflow, a cron job) cannot
//! see the status line, so this exposes the same windows as a JSON document
//! on stdout and a blocking gate that returns once every named window has
//! headroom. The OAuth token never leaves this process: the caller sees
//! numbers, not credentials.
//!
//! Cache: `usage-full.json` in the private state dir, stamped with the
//! active account and honoured for `TTL`. A `/login` to another account
//! changes the stamp, so the next read fetches immediately — the whole
//! point of switching accounts is that the new quota is visible at once.
//!
//! Wait mode sleeps in `SLICE`-long pieces between polls and re-reads the
//! account stamp each slice, so an account switch wakes the gate within a
//! slice rather than at the old account's reset time.
//!
//! Exit codes: 0 satisfied (or plain `--usage-json`), 2 no data (no token,
//! network down for `FAILURE_BUDGET`), 3 `--timeout` elapsed, 64 bad usage.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::input::RateLimitWindow;
use crate::pace;
use crate::usage::{self, FullWindows};

const TTL: Duration = Duration::from_secs(120);
/// Poll cadence while a condition is unmet and no reset is imminent.
const POLL: Duration = Duration::from_secs(300);
/// Sleep granularity — how quickly an account switch is noticed.
const SLICE: Duration = Duration::from_secs(10);
/// Consecutive fetch failures tolerated in wait mode before exit 2.
const FAILURE_BUDGET: Duration = Duration::from_secs(30 * 60);
const FIVE_HOUR_MS: i64 = 5 * 3600 * 1000;
const SEVEN_DAY_MS: i64 = 7 * 24 * 3600 * 1000;

/// One parsed `name<limit` / `name<=limit` term of `--wait-until`.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub window: Window,
    pub limit: f64,
    pub inclusive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    FiveHour,
    SevenDay,
    Fable,
    Opus,
    Sonnet,
    /// Projected end-of-window use of the 7d cap at the current rate.
    Pace,
}

impl Window {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "five_hour" | "5h" => Self::FiveHour,
            "seven_day" | "7d" => Self::SevenDay,
            "fable" => Self::Fable,
            "opus" => Self::Opus,
            "sonnet" => Self::Sonnet,
            "pace" => Self::Pace,
            _ => return None,
        })
    }
    fn name(self) -> &'static str {
        match self {
            Self::FiveHour => "five_hour",
            Self::SevenDay => "seven_day",
            Self::Fable => "fable",
            Self::Opus => "opus",
            Self::Sonnet => "sonnet",
            Self::Pace => "pace",
        }
    }
}

/// `five_hour<85,seven_day<=92,fable<95,pace<105`. Whitespace-tolerant.
pub fn parse_conditions(spec: &str) -> Result<Vec<Condition>, String> {
    let mut out = Vec::new();
    for raw in spec.split(',') {
        let term = raw.trim();
        if term.is_empty() {
            continue;
        }
        let (name, limit, inclusive) = if let Some((n, l)) = term.split_once("<=") {
            (n, l, true)
        } else if let Some((n, l)) = term.split_once('<') {
            (n, l, false)
        } else {
            return Err(format!("`{term}`: expected NAME<PERCENT or NAME<=PERCENT"));
        };
        let window = Window::parse(name.trim())
            .ok_or_else(|| format!("`{}`: unknown window (five_hour|seven_day|fable|opus|sonnet|pace)", name.trim()))?;
        let limit: f64 = limit
            .trim()
            .trim_end_matches('%')
            .parse()
            .map_err(|_| format!("`{term}`: limit is not a number"))?;
        out.push(Condition { window, limit, inclusive });
    }
    if out.is_empty() {
        return Err("no conditions given".into());
    }
    Ok(out)
}

/// A condition that does not currently hold, with the moment it would hold
/// on its own (the window's reset) when known.
#[derive(Debug, Clone, PartialEq)]
pub struct Unmet {
    pub window: &'static str,
    pub value: f64,
    pub limit: f64,
    pub resets_at_ms: Option<i64>,
}

fn window_of(w: &FullWindows, which: Window) -> Option<&RateLimitWindow> {
    match which {
        Window::FiveHour => w.five_hour.as_ref(),
        Window::SevenDay => w.seven_day.as_ref(),
        Window::Fable => w.fable.as_ref(),
        Window::Opus => w.opus.as_ref(),
        Window::Sonnet => w.sonnet.as_ref(),
        Window::Pace => w.seven_day.as_ref(),
    }
}

/// Pure: which conditions fail against `w` at `now_ms`. A window the
/// account does not have (e.g. no scoped Fable cap) satisfies any condition
/// on it — there is no limit to hit.
pub fn evaluate(w: &FullWindows, conds: &[Condition], now_ms: i64) -> Vec<Unmet> {
    let mut unmet = Vec::new();
    for c in conds {
        let Some(win) = window_of(w, c.window) else { continue };
        let value = match c.window {
            Window::Pace => match pace::pace_at(win, SEVEN_DAY_MS, now_ms).and_then(|p| p.projected) {
                Some(p) => p,
                None => continue,
            },
            _ => match win.used_percentage {
                Some(v) => v,
                None => continue,
            },
        };
        let ok = if c.inclusive { value <= c.limit } else { value < c.limit };
        if !ok {
            unmet.push(Unmet {
                window: c.window.name(),
                value,
                limit: c.limit,
                resets_at_ms: pace::reset_ms(win),
            });
        }
    }
    unmet
}

/// How long to sleep before the next poll: the regular cadence, shortened
/// to land just after the earliest reset among the unmet windows.
pub fn next_poll(unmet: &[Unmet], now_ms: i64) -> Duration {
    let earliest = unmet.iter().filter_map(|u| u.resets_at_ms).min();
    match earliest {
        Some(t) if t > now_ms => {
            let until = Duration::from_millis((t - now_ms) as u64) + Duration::from_secs(15);
            until.min(POLL)
        }
        // Already past its reset (or unknown): the next fetch will show it.
        _ => POLL.min(Duration::from_secs(60)),
    }
}

#[derive(serde::Serialize)]
struct Report<'a> {
    account: Option<&'a str>,
    fetched_at: String,
    cached: bool,
    windows: &'a FullWindows,
    seven_day_pace: Option<PaceOut>,
    five_hour_pace: Option<PaceOut>,
    earliest_reset: Option<String>,
    unmet: Vec<UnmetOut<'a>>,
}

#[derive(serde::Serialize)]
struct PaceOut {
    projected: Option<f64>,
    frac_elapsed: Option<f64>,
}

#[derive(serde::Serialize)]
struct UnmetOut<'a> {
    window: &'a str,
    value: f64,
    limit: f64,
    resets_at: Option<String>,
}

fn iso(ms: i64) -> String {
    // UTC, second precision — enough for a scheduler, no chrono dependency.
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant), valid for the Unix era.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct ProbeCache {
    account: Option<String>,
    fetched_at_ms: i64,
    windows: FullWindows,
}

/// Cached-or-fresh windows for the active account. `force` skips the cache
/// (wait mode after a sleep wants the truth, not a two-minute-old number).
fn load(force: bool) -> Option<(FullWindows, bool, i64)> {
    let account = usage::account_stamp();
    let path = usage::probe_cache_path();
    if !force
        && let Some(p) = path.as_deref()
        && usage::file_age(p).is_some_and(|a| a < TTL)
        && let Some(c) = std::fs::read(p).ok().and_then(|b| serde_json::from_slice::<ProbeCache>(&b).ok())
        && c.account == account
    {
        return Some((c.windows, true, c.fetched_at_ms));
    }
    let windows = usage::fetch_full()?;
    let fetched_at_ms = now_ms();
    if let Some(p) = path.as_deref() {
        let c = ProbeCache { account: account.clone(), fetched_at_ms, windows: windows.clone() };
        if let Ok(bytes) = serde_json::to_vec(&c) {
            let _ = usage::write_private_file(p, &bytes);
        }
    }
    Some((windows, false, fetched_at_ms))
}

fn print_report(w: &FullWindows, cached: bool, fetched_at_ms: i64, unmet: &[Unmet], now: i64) {
    let account = usage::account_stamp();
    let earliest = [&w.five_hour, &w.seven_day, &w.fable, &w.opus, &w.sonnet]
        .into_iter()
        .flatten()
        .filter_map(pace::reset_ms)
        .filter(|t| *t > now)
        .min();
    let pace_out = |win: &Option<RateLimitWindow>, len: i64| {
        win.as_ref()
            .and_then(|x| pace::pace_at(x, len, now))
            .map(|p| PaceOut { projected: p.projected, frac_elapsed: p.frac_elapsed })
    };
    let report = Report {
        account: account.as_deref(),
        fetched_at: iso(fetched_at_ms),
        cached,
        windows: w,
        seven_day_pace: pace_out(&w.seven_day, SEVEN_DAY_MS),
        five_hour_pace: pace_out(&w.five_hour, FIVE_HOUR_MS),
        earliest_reset: earliest.map(iso),
        unmet: unmet
            .iter()
            .map(|u| UnmetOut { window: u.window, value: u.value, limit: u.limit, resets_at: u.resets_at_ms.map(iso) })
            .collect(),
    };
    if let Ok(s) = serde_json::to_string(&report) {
        println!("{s}");
    }
}

/// `statusline --usage-json`
pub fn run_usage_json() -> i32 {
    match load(false) {
        Some((w, cached, at)) => {
            print_report(&w, cached, at, &[], now_ms());
            0
        }
        None => {
            eprintln!("usage-json: no data (no live OAuth token, STATUSLINE_USAGE_SOURCE=off, or fetch failed)");
            2
        }
    }
}

/// `statusline --wait-until SPEC [--timeout SECS]`
pub fn run_wait_until(spec: &str, timeout: Option<u64>) -> i32 {
    let conds = match parse_conditions(spec) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wait-until: {e}");
            return 64;
        }
    };
    let start = now_ms();
    let deadline = timeout.map(|t| start + (t as i64) * 1000);
    let mut account = usage::account_stamp();
    let mut first_failure: Option<i64> = None;
    let mut force = false;
    loop {
        let now = now_ms();
        match load(force) {
            Some((w, cached, at)) => {
                first_failure = None;
                let unmet = evaluate(&w, &conds, now);
                if unmet.is_empty() {
                    print_report(&w, cached, at, &unmet, now);
                    return 0;
                }
                let wait = next_poll(&unmet, now);
                for u in &unmet {
                    eprintln!(
                        "wait-until: {} {:.1}% ≥ {:.1}%{}",
                        u.window,
                        u.value,
                        u.limit,
                        u.resets_at_ms.map(|t| format!(", resets {}", iso(t))).unwrap_or_default()
                    );
                }
                eprintln!("wait-until: sleeping {}s", wait.as_secs());
                if sleep_watching_account(wait, &mut account, deadline) {
                    eprintln!("wait-until: account changed — re-checking the new quota");
                }
            }
            None => {
                let ff = *first_failure.get_or_insert(now);
                if now - ff > FAILURE_BUDGET.as_millis() as i64 {
                    eprintln!("wait-until: no data for {}s — giving up", FAILURE_BUDGET.as_secs());
                    return 2;
                }
                eprintln!("wait-until: fetch failed, retrying in 60s");
                sleep_watching_account(Duration::from_secs(60), &mut account, deadline);
            }
        }
        if deadline.is_some_and(|d| now_ms() >= d) {
            eprintln!("wait-until: timeout");
            return 3;
        }
        force = true;
    }
}

/// Sleep `total` in slices; return early (true) if the active account
/// changed. Also returns early when `deadline` passes.
fn sleep_watching_account(total: Duration, account: &mut Option<String>, deadline: Option<i64>) -> bool {
    let end = now_ms() + total.as_millis() as i64;
    loop {
        let now = now_ms();
        if now >= end || deadline.is_some_and(|d| now >= d) {
            return false;
        }
        let slice = SLICE.min(Duration::from_millis((end - now) as u64));
        std::thread::sleep(slice);
        let current = usage::account_stamp();
        if current != *account {
            *account = current;
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(used: f64, resets_at: &str) -> RateLimitWindow {
        RateLimitWindow { used_percentage: Some(used), resets_at: Some(serde_json::Value::String(resets_at.into())) }
    }

    fn windows() -> FullWindows {
        FullWindows {
            five_hour: Some(win(90.0, "2026-09-05T14:00:00+00:00")),
            seven_day: Some(win(50.0, "2026-09-10T00:00:00+00:00")),
            fable: Some(win(13.0, "2026-09-07T22:00:00+00:00")),
            opus: None,
            sonnet: None,
        }
    }

    const NOW: i64 = 1_788_598_800_000; // 2026-09-05T09:00:00Z, inside every window above

    #[test]
    fn parses_conditions_with_aliases_and_inclusive() {
        let c = parse_conditions(" 5h<85, seven_day<=92 ,fable<95%").unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].window, Window::FiveHour);
        assert!(!c[0].inclusive);
        assert_eq!(c[1].window, Window::SevenDay);
        assert!(c[1].inclusive);
        assert_eq!(c[2].limit, 95.0);
    }

    #[test]
    fn rejects_bad_specs() {
        assert!(parse_conditions("five_hour>85").is_err());
        assert!(parse_conditions("nope<10").is_err());
        assert!(parse_conditions("").is_err());
        assert!(parse_conditions("fable<x").is_err());
    }

    #[test]
    fn evaluate_reports_only_failing_windows_with_reset() {
        let conds = parse_conditions("five_hour<85,seven_day<92,fable<95").unwrap();
        let unmet = evaluate(&windows(), &conds, NOW);
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].window, "five_hour");
        assert_eq!(unmet[0].value, 90.0);
        assert!(unmet[0].resets_at_ms.is_some());
    }

    #[test]
    fn evaluate_inclusive_boundary() {
        let w = FullWindows { five_hour: Some(win(85.0, "2026-09-05T14:00:00+00:00")), ..Default::default() };
        assert_eq!(evaluate(&w, &parse_conditions("5h<85").unwrap(), NOW).len(), 1);
        assert!(evaluate(&w, &parse_conditions("5h<=85").unwrap(), NOW).is_empty());
    }

    #[test]
    fn evaluate_treats_absent_window_as_satisfied() {
        // Account without a scoped Fable cap: `fable<95` cannot fail.
        let w = FullWindows { fable: None, ..windows() };
        let unmet = evaluate(&w, &parse_conditions("fable<1").unwrap(), NOW);
        assert!(unmet.is_empty());
    }

    #[test]
    fn evaluate_pace_uses_projection_not_raw_use() {
        // 50% used with the window nearly over → projected ≈ 50, passes 105.
        let late = FullWindows { seven_day: Some(win(50.0, "2026-09-05T12:00:00+00:00")), ..Default::default() };
        assert!(evaluate(&late, &parse_conditions("pace<105").unwrap(), NOW).is_empty());
        // 50% used with ~6 days left → projected far above 105, fails.
        let early = FullWindows { seven_day: Some(win(50.0, "2026-09-11T12:00:00+00:00")), ..Default::default() };
        let unmet = evaluate(&early, &parse_conditions("pace<105").unwrap(), NOW);
        assert_eq!(unmet.len(), 1);
        assert!(unmet[0].value > 105.0);
    }

    #[test]
    fn next_poll_shortens_to_the_earliest_reset() {
        let soon = Unmet { window: "five_hour", value: 90.0, limit: 85.0, resets_at_ms: Some(NOW + 60_000) };
        assert_eq!(next_poll(&[soon], NOW), Duration::from_secs(75));
        let far = Unmet { window: "seven_day", value: 99.0, limit: 92.0, resets_at_ms: Some(NOW + 86_400_000) };
        assert_eq!(next_poll(&[far], NOW), POLL);
        let past = Unmet { window: "fable", value: 99.0, limit: 92.0, resets_at_ms: Some(NOW - 1) };
        assert_eq!(next_poll(&[past], NOW), Duration::from_secs(60));
        let unknown = Unmet { window: "fable", value: 99.0, limit: 92.0, resets_at_ms: None };
        assert_eq!(next_poll(&[unknown], NOW), Duration::from_secs(60));
    }

    #[test]
    fn iso_round_trips_through_the_parser() {
        for ms in [0i64, NOW, 4_102_444_800_000] {
            let s = iso(ms);
            assert_eq!(crate::format::parse_rfc3339_ms(&s), Some(ms), "{s}");
        }
        assert_eq!(iso(NOW), "2026-09-05T09:00:00Z");
    }
}
