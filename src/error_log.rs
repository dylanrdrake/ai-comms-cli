//! A small rolling debug log at `~/.clank/errors.log`, so a confusing API or
//! provider error can be looked back at (or pasted somewhere for help)
//! without having to catch and copy it in the moment it happened.
//!
//! Deliberately not a general-purpose logging framework — just one
//! function, called from `client.rs` wherever a request to the provider
//! fails (a non-2xx response, a stalled or dropped connection, a malformed
//! stream), keeping the most recent 100 entries. Those are the errors worth
//! a second look: they arrive as a wall of provider JSON, scroll away, and
//! often can't be reproduced on demand.

use crate::config::get_config_dir;
use anyhow::Result;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_ENTRIES: usize = 100;
const LOG_FILE: &str = "errors.log";
const REQUEST_DUMP_FILE: &str = "failed-request.json";
const REQUEST_DUMP_VAR: &str = "CLANK_DEBUG_REQUESTS";

/// Appends one entry and trims the file back down to the most recent
/// [`MAX_ENTRIES`]. Best-effort: a failure here (a full disk, a permissions
/// problem) is swallowed rather than interrupting whatever real work
/// triggered the error being logged — this is a debugging aid, not a
/// critical path.
pub fn log_error(context: &str, message: &str) {
    let _ = try_log_error(context, message);
}

fn try_log_error(context: &str, message: &str) -> Result<()> {
    append_entry(&get_config_dir()?, context, message)
}

/// The body of [`try_log_error`], against an explicit directory.
///
/// Split out so a test can hand it a temp directory rather than trying to
/// move the real one. Redirecting `get_config_dir` means overriding `HOME`,
/// which only works on Unix — on Windows the home directory comes from
/// `USERPROFILE`, so the test wrote to the developer's actual profile and
/// then failed looking for the file somewhere else. It also made the test
/// mutate process-wide state that every other test shares.
fn append_entry(dir: &Path, context: &str, message: &str) -> Result<()> {
    let path = dir.join(LOG_FILE);

    let mut lines: Vec<String> = if path.exists() {
        fs::read_to_string(&path)?
            .lines()
            .map(str::to_string)
            .collect()
    } else {
        Vec::new()
    };

    lines.push(format_entry(context, message));
    if lines.len() > MAX_ENTRIES {
        let excess = lines.len() - MAX_ENTRIES;
        lines.drain(..excess);
    }

    let mut file = fs::File::create(&path)?;
    for line in &lines {
        writeln!(file, "{line}")?;
    }
    Ok(())
}

/// One log line: `[<UTC timestamp>] <context>: <message>`. `message` is
/// flattened to a single line — an API's pretty-printed JSON body or a
/// Rust error chain's "Caused by:" lines read fine collapsed with `; `,
/// and it keeps trimming the file down to entries as simple as counting
/// lines, rather than needing a real multi-line-record delimiter.
fn format_entry(context: &str, message: &str) -> String {
    let flat = message
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    format!("[{}] {context}: {flat}", timestamp())
}

/// `YYYY-MM-DDTHH:MM:SSZ` in UTC, from the system clock. No date/time
/// dependency for this — see [`civil_from_days`].
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let time_of_day = secs % 86400;
    let (hour, minute, second) = (
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60,
    );
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Converts a day count since the Unix epoch (1970-01-01) into a
/// `(year, month, day)` civil (Gregorian) date, in UTC. Howard Hinnant's
/// widely-used `civil_from_days` algorithm, correct proleptic-Gregorian for
/// any date a process could plausibly log — pulling in a date/time crate
/// for one timestamp format isn't worth it.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Whether raw request capture is switched on, via `CLANK_DEBUG_REQUESTS=1`.
/// Checked before serializing anything, since the body is the whole
/// conversation and building it isn't free.
pub fn request_dumps_enabled() -> bool {
    matches!(
        std::env::var(REQUEST_DUMP_VAR).as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Writes the exact JSON body of a failed request to
/// `~/.clank/failed-request.json`, overwriting any previous one, and returns
/// where it went.
///
/// This is the deliberate opposite of [`log_error`]'s content-free
/// discipline: it holds the **entire conversation** — every message, tool
/// call and tool result, verbatim. That's the point. A provider that
/// rejects a request by message index ("messages.111: The final block in an
/// assistant message cannot be `thinking`") can only be argued with by
/// reading message 111, and the index is into the array *after* the
/// provider's own translation, so it can't be reconstructed from a summary.
///
/// Because it's the conversation, it's off unless [`request_dumps_enabled`]
/// says otherwise, it keeps only the most recent failure rather than
/// accumulating, and it lives under an obvious name so it's easy to find
/// and delete.
pub fn dump_failed_request(body: &str) -> Option<PathBuf> {
    let path = get_config_dir().ok()?.join(REQUEST_DUMP_FILE);
    fs::write(&path, body).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_dates() {
        // Epoch itself.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // A leap day, and the day after it.
        assert_eq!(civil_from_days(19782), (2024, 2, 29));
        assert_eq!(civil_from_days(19783), (2024, 3, 1));
        // A century non-leap year boundary (1900 isn't a leap year, but
        // this crosses 2000, which is).
        assert_eq!(civil_from_days(10957), (2000, 1, 1));
        // New Year's Eve into New Year's Day.
        assert_eq!(civil_from_days(11322), (2000, 12, 31));
        assert_eq!(civil_from_days(11323), (2001, 1, 1));
    }

    #[test]
    fn multiline_messages_are_flattened_to_one_entry() {
        let entry = format_entry("test", "line one\n  line two  \n\nline three");
        assert_eq!(entry.matches('\n').count(), 0);
        assert!(entry.ends_with("test: line one; line two; line three"));
    }

    #[test]
    fn log_error_trims_to_the_most_recent_entries() {
        let dir = tempfile_dir();

        for i in 0..(MAX_ENTRIES + 10) {
            append_entry(&dir, "test", &format!("entry {i}")).unwrap();
        }

        let path = dir.join(LOG_FILE);
        let contents = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), MAX_ENTRIES);
        // The oldest entries were dropped; the newest survived, in order.
        assert!(lines[0].ends_with("entry 10"));
        assert!(lines[lines.len() - 1].ends_with(&format!("entry {}", MAX_ENTRIES + 9)));

        fs::remove_dir_all(&dir).ok();
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clank-error-log-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
