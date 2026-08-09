use std::os::unix::net::UnixListener;
use std::thread;

use super::*;

#[test]
fn parses_the_herdr_0_7_5_tab_get_response() {
    let result: TabGetResult = serde_json::from_str(
        r#"{
            "type": "tab_info",
            "tab": {
                "tab_id": "wA:t1F",
                "workspace_id": "wA",
                "number": 47,
                "label": "[7] reviewr",
                "focused": false,
                "pane_count": 1,
                "agent_status": "unknown"
            }
        }"#,
    )
    .unwrap();

    assert_eq!(
        Tab::from(result.tab),
        Tab {
            tab_id: "wA:t1F".into(),
            workspace_id: "wA".into(),
            label: "[7] reviewr".into(),
        }
    );
}

#[test]
fn requests_and_parses_a_session_snapshot() {
    let socket_path = std::env::temp_dir().join(format!(
        "herdr-labels-snapshot-test-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let request: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["method"], "session.snapshot");
        let response = json!({
            "id": request["id"],
            "result": {
                "type": "session_snapshot",
                "snapshot": {
                    "focused_pane_id": "w1:p1",
                    "tabs": [{
                        "tab_id": "w1:t1", "workspace_id": "w1", "label": "1",
                        "focused": true, "pane_count": 1
                    }],
                    "panes": [{"pane_id": "w1:p1", "tab_id": "w1:t1"}]
                }
            }
        });
        writeln!(stream, "{response}").unwrap();
    });

    let mut client = HerdrClient::new(&socket_path);
    let snapshot = client.snapshot().unwrap();
    assert_eq!(snapshot.focused_pane_id.as_deref(), Some("w1:p1"));
    assert_eq!(snapshot.tabs[0].tab.label, "1");
    assert_eq!(snapshot.panes[0].pane_id, "w1:p1");
    server.join().unwrap();
    std::fs::remove_file(socket_path).unwrap();
}

#[test]
fn requests_and_parses_foreground_process_information() {
    let socket_path = std::env::temp_dir().join(format!(
        "herdr-labels-process-test-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let request: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["method"], "pane.process_info");
        assert_eq!(request["params"]["pane_id"], "w1:p1");
        let response = json!({
            "id": request["id"],
            "result": {
                "type": "pane_process_info",
                "process_info": {
                    "pane_id": "w1:p1",
                    "foreground_process_group_id": 42,
                    "foreground_processes": [{
                        "pid": 42, "name": "wrapped", "argv0": "/usr/bin/nvim",
                        "argv": ["nvim"]
                    }]
                }
            }
        });
        writeln!(stream, "{response}").unwrap();
    });

    let mut client = HerdrClient::new(&socket_path);
    let process_info = client.pane_process_info("w1:p1").unwrap();
    assert_eq!(process_info.leader().unwrap().program(), "/usr/bin/nvim");
    server.join().unwrap();
    std::fs::remove_file(socket_path).unwrap();
}
