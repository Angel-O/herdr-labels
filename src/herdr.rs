//! Timeout-bounded client for the subset of Herdr's socket API used here.

use std::error::Error;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::numbering::Tab;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);

/// Client for issuing tab requests to one Herdr session.
///
/// Herdr serves one request per socket connection, so each method opens a fresh
/// connection with bounded read and write waits.
pub(crate) struct HerdrClient {
    socket_path: PathBuf,
    next_request_id: u64,
}

impl HerdrClient {
    /// Creates a client targeting the supplied Herdr socket.
    pub(crate) fn new(socket_path: &Path) -> Self {
        Self {
            socket_path: socket_path.to_owned(),
            next_request_id: 1,
        }
    }

    /// Returns the session topology used to resolve a tab's naming pane.
    pub(crate) fn snapshot(&mut self) -> Result<SessionSnapshot> {
        let result: SessionSnapshotResult = self.request("session.snapshot", json!({}))??;
        if result.kind != "session_snapshot" {
            return Err(format!(
                "session snapshot returned unexpected result type {:?}",
                result.kind
            )
            .into());
        }
        Ok(SessionSnapshot {
            focused_pane_id: result.snapshot.focused_pane_id,
            tabs: result
                .snapshot
                .tabs
                .into_iter()
                .map(|tab| SessionTab {
                    focused: tab.focused,
                    pane_count: tab.pane_count,
                    tab: tab.into(),
                })
                .collect(),
            panes: result.snapshot.panes,
        })
    }

    /// Returns foreground process information for one pane.
    pub(crate) fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo> {
        let result: PaneProcessInfoResult =
            self.request("pane.process_info", json!({ "pane_id": pane_id }))??;
        if result.kind != "pane_process_info" {
            return Err(format!(
                "pane process info returned unexpected result type {:?}",
                result.kind
            )
            .into());
        }
        Ok(result.process_info)
    }

    /// Returns current state for a tab, or `None` if it disappeared.
    ///
    /// # Errors
    ///
    /// Returns an error for failures other than Herdr's `tab_not_found` result.
    pub(crate) fn get_tab(&mut self, tab_id: &str) -> Result<Option<Tab>> {
        let result: std::result::Result<TabGetResult, HerdrApiError> =
            self.request("tab.get", json!({ "tab_id": tab_id }))?;
        match result {
            Ok(result) if result.kind == "tab_info" => Ok(Some(result.tab.into())),
            Ok(result) => {
                Err(format!("tab get returned unexpected result type {:?}", result.kind).into())
            }
            Err(error) if error.code == "tab_not_found" => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Replaces a tab's label, treating concurrent tab removal as benign.
    ///
    /// # Errors
    ///
    /// Returns an error for failures other than Herdr's `tab_not_found` result.
    pub(crate) fn rename_tab(&mut self, tab_id: &str, label: &str) -> Result<()> {
        let result: std::result::Result<TabGetResult, HerdrApiError> =
            self.request("tab.rename", json!({ "tab_id": tab_id, "label": label }))?;
        match result {
            Ok(result) if result.kind == "tab_info" => Ok(()),
            Ok(result) => Err(format!(
                "tab rename returned unexpected result type {:?}",
                result.kind
            )
            .into()),
            Err(error) if error.code == "tab_not_found" => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn request<T: for<'de> Deserialize<'de>>(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<std::result::Result<T, HerdrApiError>> {
        let request_id = format!("tabs:{}", self.next_request_id);
        self.next_request_id += 1;
        let mut stream = UnixStream::connect(&self.socket_path)?;
        stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
        stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
        serde_json::to_writer(
            &mut stream,
            &json!({ "id": request_id, "method": method, "params": params }),
        )?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Herdr closed the socket before responding",
            )
            .into());
        }
        let response: HerdrResponse<T> = serde_json::from_str(&line)?;
        if response.id != request_id {
            return Err(format!(
                "Herdr response id {:?} did not match request id {:?}",
                response.id, request_id
            )
            .into());
        }
        match (response.result, response.error) {
            (Some(result), None) => Ok(Ok(result)),
            (None, Some(error)) => Ok(Err(error)),
            _ => Err("Herdr response contained neither a result nor an error".into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct HerdrResponse<T> {
    id: String,
    result: Option<T>,
    error: Option<HerdrApiError>,
}

#[derive(Debug, Deserialize)]
struct TabGetResult {
    #[serde(rename = "type")]
    kind: String,
    tab: HerdrTab,
}

/// Session topology needed by automatic naming.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionSnapshot {
    pub(crate) focused_pane_id: Option<String>,
    pub(crate) tabs: Vec<SessionTab>,
    pub(crate) panes: Vec<PaneInfo>,
}

/// Tab state plus pane-selection fields from a session snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionTab {
    pub(crate) tab: Tab,
    pub(crate) focused: bool,
    pub(crate) pane_count: usize,
}

/// Pane membership fields from a session snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct PaneInfo {
    pub(crate) pane_id: String,
    pub(crate) tab_id: String,
}

/// Foreground processes associated with one pane.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct PaneProcessInfo {
    pub(crate) foreground_process_group_id: Option<u32>,
    #[serde(default)]
    pub(crate) foreground_processes: Vec<ProcessInfo>,
}

impl PaneProcessInfo {
    /// Selects the foreground process-group leader when Herdr reports one.
    pub(crate) fn leader(&self) -> Option<&ProcessInfo> {
        let group = self.foreground_process_group_id?;
        self.foreground_processes
            .iter()
            .find(|process| process.pid == group)
    }
}

/// Process identity fields used to derive a tab name.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct ProcessInfo {
    pub(crate) pid: u32,
    pub(crate) name: String,
    pub(crate) argv0: Option<String>,
    pub(crate) argv: Option<Vec<String>>,
}

impl ProcessInfo {
    /// Returns the invoked program name, preferring argv data over the executable name.
    pub(crate) fn program(&self) -> &str {
        self.argv0
            .as_deref()
            .filter(|program| !program.is_empty())
            .or_else(|| self.argv.as_deref()?.first().map(String::as_str))
            .filter(|program| !program.is_empty())
            .unwrap_or(&self.name)
    }
}

#[derive(Debug, Deserialize)]
struct SessionSnapshotResult {
    #[serde(rename = "type")]
    kind: String,
    snapshot: SessionSnapshotWire,
}

#[derive(Debug, Deserialize)]
struct SessionSnapshotWire {
    focused_pane_id: Option<String>,
    tabs: Vec<HerdrTab>,
    panes: Vec<PaneInfo>,
}

#[derive(Debug, Deserialize)]
struct PaneProcessInfoResult {
    #[serde(rename = "type")]
    kind: String,
    process_info: PaneProcessInfo,
}

#[derive(Debug, Deserialize)]
struct HerdrApiError {
    code: String,
    message: String,
}

impl std::fmt::Display for HerdrApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Herdr API error {}: {}", self.code, self.message)
    }
}

impl Error for HerdrApiError {}

#[derive(Debug, Deserialize)]
struct HerdrTab {
    tab_id: String,
    workspace_id: String,
    label: String,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    pane_count: usize,
}

impl From<HerdrTab> for Tab {
    fn from(tab: HerdrTab) -> Self {
        Self {
            tab_id: tab.tab_id,
            workspace_id: tab.workspace_id,
            label: tab.label,
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/herdr.rs"]
mod tests;
