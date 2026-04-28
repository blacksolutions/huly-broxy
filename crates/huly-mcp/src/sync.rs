//! Subprocess wrapper around the upstream Huly Node.js sync pipeline.
//!
//! Two operations are exposed:
//!
//! * [`SyncRunner::status`] — compares local docs against `.huly-sync-state.json`
//!   and reports new / modified / deleted files. Implemented by shelling out to
//!   `node -e <inline JS>` so we get Node's `crypto.md5` + `fs` walking without
//!   adding Rust dependencies. The upstream sync CLI does *not* expose a
//!   `status` subcommand — the upstream MCP wrapper at
//!   `huly-api/packages/mcp-server/src/tools/sync.ts` re-implements it inline,
//!   and we mirror that approach here.
//!
//! * [`SyncRunner::sync`] — runs the full Enums → MasterTags → Associations →
//!   Cards → Binaries → Relations pipeline. Spawns `node {script_path}` with
//!   an optional `--dry-run` flag, exactly matching the upstream invocation.
//!
//! Both subprocesses are bounded by [`SyncConfig::timeout_secs`] and killed on
//! drop. No new external Rust crates are required — `tokio::process` and
//! `serde_json` carry the load.

use crate::config::SyncConfig;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::process::Command;

/// Inline Node.js script that replicates the upstream MCP wrapper's status
/// logic. Reads `.huly-sync-state.json` from CWD, walks the import path
/// (passed via `HULY_IMPORT_PATH` env var, defaults to `docs`), MD5-hashes
/// each file, and prints a JSON report on stdout.
const STATUS_SCRIPT: &str = r#"
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const importPath = process.env.HULY_IMPORT_PATH || 'docs';
const statePath = process.env.HULY_SYNC_STATE_PATH || '.huly-sync-state.json';
let state = { files: {} };
if (fs.existsSync(statePath)) {
  state = JSON.parse(fs.readFileSync(statePath, 'utf-8'));
}
const ignored = new Set(['.claude', 'Dev', 'node_modules', '.git', 'diagrams']);
function walk(dir) {
  const out = [];
  if (!fs.existsSync(dir)) return out;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (ignored.has(e.name)) continue;
    const full = path.join(dir, e.name);
    if (e.isDirectory()) out.push(...walk(full));
    else out.push(full);
  }
  return out;
}
const diskFiles = walk(importPath).map(f => path.relative(importPath, f));
const newFiles = [], modified = [], deleted = [];
for (const rel of diskFiles) {
  const entry = state.files ? state.files[rel] : null;
  if (entry == null) {
    newFiles.push(rel);
  } else {
    const h = crypto.createHash('md5').update(fs.readFileSync(path.join(importPath, rel))).digest('hex');
    if (h !== entry.contentHash) modified.push(rel);
  }
}
const diskSet = new Set(diskFiles);
for (const rel of Object.keys(state.files || {})) {
  if (!diskSet.has(rel)) deleted.push(rel);
}
const total = newFiles.length + modified.length + deleted.length;
const summary = total === 0 ? 'Everything is in sync. No changes detected.' : (total + ' changes detected.');
process.stdout.write(JSON.stringify({
  summary,
  lastSync: state.lastSync || 'never',
  new: newFiles,
  modified,
  deleted,
  totalTracked: Object.keys(state.files || {}).length,
  totalOnDisk: diskFiles.length,
}, null, 2));
"#;

/// Parsed status report mirroring the upstream JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusReport {
    pub summary: String,
    #[serde(rename = "lastSync")]
    pub last_sync: String,
    pub new: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    #[serde(rename = "totalTracked")]
    pub total_tracked: u64,
    #[serde(rename = "totalOnDisk")]
    pub total_on_disk: u64,
}

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("failed to spawn sync subprocess ({command}): {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("sync subprocess timed out after {0:?}")]
    Timeout(Duration),

    #[error("sync subprocess failed (exit {code}): {stderr_tail}")]
    NonZeroExit { code: i32, stderr_tail: String },

    #[error("failed to parse sync status output as JSON: {source}\nraw stdout: {raw}")]
    ParseStatus {
        #[source]
        source: serde_json::Error,
        raw: String,
    },
}

/// Outcome of running a sync. Keeps stdout and stderr distinct so callers can
/// decide how to render them.
#[derive(Debug, Clone)]
pub struct SyncOutput {
    pub stdout: String,
    pub stderr: String,
}

/// Subprocess driver around the Node.js sync tool.
#[derive(Debug, Clone)]
pub struct SyncRunner {
    script_path: PathBuf,
    node_binary: String,
    working_dir: PathBuf,
    timeout: Duration,
}

impl SyncRunner {
    /// Build a runner from a parsed [`SyncConfig`]. Returns `None` when sync
    /// is not configured — callers should surface the dedicated
    /// `SyncError::not_configured()` text instead of constructing a runner.
    pub fn new(config: &SyncConfig) -> Self {
        Self {
            script_path: config.script_path.clone(),
            node_binary: config.node_binary.clone(),
            working_dir: config.working_dir.clone(),
            timeout: Duration::from_secs(config.timeout_secs),
        }
    }

    /// Standard error message returned by tool handlers when no `[mcp.sync]`
    /// section is configured.
    pub fn not_configured_error() -> String {
        "Sync is not configured. Set [mcp.sync] script_path = \"/path/to/huly-api/packages/sync/dist/index.js\" in the MCP config TOML to enable huly_sync_status and huly_sync_cards.".to_string()
    }

    /// Args used for the status invocation. Exposed for tests; in production
    /// `node -e <inline-js>` is launched, with state/disk paths injected via
    /// env vars.
    fn status_args(&self) -> Vec<OsString> {
        vec!["-e".into(), STATUS_SCRIPT.into()]
    }

    /// Args used for the sync invocation.
    fn sync_args(&self, dry_run: bool) -> Vec<OsString> {
        let mut v: Vec<OsString> = vec![self.script_path.clone().into()];
        if dry_run {
            v.push("--dry-run".into());
        }
        v
    }

    /// Spawn the configured node binary with `args`, capture stdout+stderr,
    /// enforce the timeout, and validate the exit status.
    async fn spawn_capture(&self, args: Vec<OsString>) -> Result<SyncOutput, SyncError> {
        let mut cmd = Command::new(&self.node_binary);
        cmd.args(&args)
            .current_dir(&self.working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Tell the inline status script which dir to walk (matches upstream
        // HULY_SYNC_PATH behaviour). For sync runs this var is harmless.
        cmd.env(
            "HULY_IMPORT_PATH",
            self.working_dir.join("docs").as_os_str(),
        );
        cmd.env(
            "HULY_SYNC_STATE_PATH",
            self.working_dir.join(".huly-sync-state.json").as_os_str(),
        );

        let cmd_str = format!("{} {:?}", self.node_binary, args);

        let child = cmd.spawn().map_err(|e| SyncError::Spawn {
            command: cmd_str.clone(),
            source: e,
        })?;

        let output_fut = child.wait_with_output();
        let output = match tokio::time::timeout(self.timeout, output_fut).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return Err(SyncError::Spawn {
                    command: cmd_str,
                    source: e,
                });
            }
            Err(_) => return Err(SyncError::Timeout(self.timeout)),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            // Combine stderr + stdout tail for context, capped at ~2KB.
            let mut tail = stderr.clone();
            if !stdout.is_empty() {
                tail.push('\n');
                tail.push_str(&stdout);
            }
            if tail.len() > 2048 {
                tail = tail.split_off(tail.len() - 2048);
            }
            return Err(SyncError::NonZeroExit {
                code,
                stderr_tail: tail,
            });
        }

        Ok(SyncOutput { stdout, stderr })
    }

    /// Run the status check and parse its JSON output.
    pub async fn status(&self) -> Result<StatusReport, SyncError> {
        let out = self.spawn_capture(self.status_args()).await?;
        serde_json::from_str(out.stdout.trim()).map_err(|e| SyncError::ParseStatus {
            source: e,
            raw: out.stdout,
        })
    }

    /// Run the sync pipeline. Returns the captured stdout/stderr; callers can
    /// filter / format as needed.
    pub async fn sync(&self, dry_run: bool) -> Result<SyncOutput, SyncError> {
        self.spawn_capture(self.sync_args(dry_run)).await
    }

    /// Filter upstream's noisy "no document found" lines out of `stdout`,
    /// matching the upstream MCP wrapper's behaviour.
    pub fn filter_sync_output(stdout: &str) -> String {
        stdout
            .lines()
            .filter(|l| !l.contains("no document found"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn fixture_path(name: &str) -> PathBuf {
        let manifest = env!("CARGO_MANIFEST_DIR");
        PathBuf::from(manifest).join("tests/fixtures").join(name)
    }

    fn temp_workdir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "huly-sync-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn runner_with(node_binary: PathBuf, working_dir: PathBuf, timeout_secs: u64) -> SyncRunner {
        SyncRunner {
            script_path: PathBuf::from("/fake/sync/dist/index.js"),
            node_binary: node_binary.to_string_lossy().into_owned(),
            working_dir,
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    #[test]
    fn not_configured_error_mentions_script_path() {
        let msg = SyncRunner::not_configured_error();
        assert!(msg.contains("script_path"));
        assert!(msg.contains("[mcp.sync]"));
    }

    #[test]
    fn status_args_use_dash_e_and_inline_script() {
        let r = runner_with(PathBuf::from("node"), temp_workdir("status-args"), 1);
        let args = r.status_args();
        assert_eq!(args[0], "-e");
        assert!(args[1].to_string_lossy().contains("crypto.createHash"));
    }

    #[test]
    fn sync_args_no_dry_run_passes_only_script_path() {
        let r = runner_with(PathBuf::from("node"), temp_workdir("sync-args"), 1);
        let args = r.sync_args(false);
        assert_eq!(args.len(), 1);
        assert_eq!(args[0], "/fake/sync/dist/index.js");
    }

    #[test]
    fn sync_args_dry_run_appends_flag() {
        let r = runner_with(PathBuf::from("node"), temp_workdir("sync-dry-args"), 1);
        let args = r.sync_args(true);
        assert_eq!(args.len(), 2);
        assert_eq!(args[1], "--dry-run");
    }

    #[test]
    fn filter_sync_output_drops_noise() {
        let s = "ok line\n[Scan] no document found for foo\nanother ok";
        let filtered = SyncRunner::filter_sync_output(s);
        assert!(!filtered.contains("no document found"));
        assert!(filtered.contains("ok line"));
        assert!(filtered.contains("another ok"));
    }

    #[tokio::test]
    async fn status_happy_path_parses_canned_json() {
        let workdir = temp_workdir("status-happy");
        let r = runner_with(fixture_path("fake_sync_status.sh"), workdir.clone(), 5);

        let report = r.status().await.expect("status should succeed");

        assert_eq!(report.summary, "2 changes detected.");
        assert_eq!(report.new, vec!["docs/new.md"]);
        assert_eq!(report.modified, vec!["docs/changed.md"]);
        assert_eq!(report.total_tracked, 5);
        assert_eq!(report.total_on_disk, 6);

        // Verify the runner spawned with `-e <inline JS>`. The stub records
        // each arg followed by a newline; the inline JS itself contains
        // newlines, so we check the whole recorded blob for the marker.
        let recorded = std::fs::read_to_string(workdir.join("args.txt")).unwrap();
        assert!(recorded.starts_with("-e\n"), "first arg must be -e");
        assert!(
            recorded.contains("crypto.createHash"),
            "inline script must invoke node crypto"
        );
    }

    #[tokio::test]
    async fn sync_happy_path_no_dry_run() {
        let workdir = temp_workdir("sync-happy");
        let r = runner_with(fixture_path("fake_sync_ok.sh"), workdir.clone(), 5);

        let out = r.sync(false).await.expect("sync should succeed");

        assert!(out.stdout.contains("Sync OK"));
        let recorded = std::fs::read_to_string(workdir.join("args.txt")).unwrap();
        let lines: Vec<&str> = recorded.lines().collect();
        assert_eq!(lines, vec!["/fake/sync/dist/index.js"]);
    }

    #[tokio::test]
    async fn sync_dry_run_appends_flag_in_subprocess() {
        let workdir = temp_workdir("sync-dry");
        let r = runner_with(fixture_path("fake_sync_ok.sh"), workdir.clone(), 5);

        let out = r.sync(true).await.expect("sync should succeed");

        assert!(out.stdout.contains("DRY RUN"));
        let recorded = std::fs::read_to_string(workdir.join("args.txt")).unwrap();
        let lines: Vec<&str> = recorded.lines().collect();
        assert_eq!(lines, vec!["/fake/sync/dist/index.js", "--dry-run"]);
    }

    #[tokio::test]
    async fn sync_non_zero_exit_returns_error_with_stderr_tail() {
        let workdir = temp_workdir("sync-fail");
        let r = runner_with(fixture_path("fake_sync_fail.sh"), workdir, 5);

        let err = r.sync(false).await.expect_err("should fail");
        match err {
            SyncError::NonZeroExit { code, stderr_tail } => {
                assert_eq!(code, 7);
                assert!(stderr_tail.contains("connection refused"));
            }
            other => panic!("expected NonZeroExit, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn status_invalid_json_returns_parse_error() {
        // Use the OK stub (prints non-JSON) for the status path.
        let workdir = temp_workdir("status-bad");
        let r = runner_with(fixture_path("fake_sync_ok.sh"), workdir, 5);
        let err = r.status().await.expect_err("should not parse");
        match err {
            SyncError::ParseStatus { raw, .. } => assert!(raw.contains("Sync OK")),
            other => panic!("expected ParseStatus, got {:?}", other),
        }
    }
}
