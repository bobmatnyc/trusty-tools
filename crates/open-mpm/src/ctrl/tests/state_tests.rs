//! Tests for `Ctrl` lifecycle (prompt, connect, dispatch, shutdown) and the
//! `PmHandle` actor loop.

use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use super::super::state::{Ctrl, PmHandle, PmMsg};
use super::{insert_fake, insert_mock_actor};

// -- Ctrl::new --
#[test]
fn new_ctrl_has_no_sessions() {
    let c = Ctrl::new();
    assert!(c.pms.is_empty() && c.active.is_none());
}

// -- Ctrl::prompt --
#[test]
fn prompt_without_active_shows_ctrl() {
    assert_eq!(Ctrl::new().prompt(), "CTRL> ");
}

#[tokio::test]
async fn prompt_with_active_shows_pm_name() {
    let mut c = Ctrl::new();
    insert_fake(&mut c, "/tmp/p", "proj");
    c.active = Some("/tmp/p".to_string());
    assert_eq!(c.prompt(), "PM[proj]> ");
}

#[test]
fn prompt_with_stale_active_shows_question_mark() {
    let mut c = Ctrl::new();
    c.active = Some("/gone".to_string());
    assert_eq!(c.prompt(), "PM[?]> ");
}

// -- Ctrl::disconnect --
#[test]
fn disconnect_when_no_active() {
    let mut c = Ctrl::new();
    assert_eq!(c.disconnect(), "No active PM session.");
}

#[tokio::test]
async fn disconnect_clears_active_but_keeps_handle() {
    let mut c = Ctrl::new();
    insert_fake(&mut c, "/tmp/p", "proj");
    c.active = Some("/tmp/p".to_string());
    let msg = c.disconnect();
    assert!(msg.contains("Disconnected") && msg.contains("proj"));
    assert!(c.active.is_none() && c.pms.contains_key("/tmp/p"));
}

// -- Ctrl::status --
#[test]
fn status_empty() {
    assert_eq!(Ctrl::new().status(), "No PM sessions.");
}

#[tokio::test]
async fn status_lists_sessions_with_markers() {
    let mut c = Ctrl::new();
    insert_fake(&mut c, "/a", "alpha");
    insert_fake(&mut c, "/b", "beta");
    c.active = Some("/a".to_string());
    let out = c.status();
    assert!(out.contains("alpha") && out.contains("beta"));
    assert!(out.contains("[*]") && out.contains("[ ]"));
}

// -- Ctrl::dispatch_task --
#[tokio::test]
async fn dispatch_without_active_errors() {
    let e = Ctrl::new().dispatch_task("hi".into()).await.unwrap_err();
    assert!(e.to_string().contains("no active PM session"));
}

#[tokio::test]
async fn dispatch_with_closed_channel_errors() {
    let mut c = Ctrl::new();
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    c.pms.insert(
        "/d".into(),
        PmHandle {
            name: "d".into(),
            project_path: "/d".into(),
            tx,
            task: tokio::spawn(async {}),
            status: Arc::new(Mutex::new("idle".to_string())),
            last_message: Arc::new(Mutex::new(String::new())),
        },
    );
    c.active = Some("/d".into());
    assert!(c.dispatch_task("hi".into()).await.is_err());
}

#[tokio::test]
async fn dispatch_receives_actor_reply() {
    let mut c = Ctrl::new();
    insert_mock_actor(&mut c, "/m", Ok("ok".into()));
    assert_eq!(c.dispatch_task("hi".into()).await.unwrap(), "ok");
}

#[tokio::test]
async fn dispatch_propagates_actor_error() {
    let mut c = Ctrl::new();
    insert_mock_actor(&mut c, "/e", Err(anyhow::anyhow!("boom")));
    let e = c.dispatch_task("hi".into()).await.unwrap_err();
    assert!(e.to_string().contains("boom"));
}

// -- Ctrl::connect --
#[tokio::test]
async fn connect_creates_handle() {
    let mut c = Ctrl::new();
    let tmp = tempfile::tempdir().unwrap();
    let msg = c.connect(tmp.path().to_str().unwrap()).await.unwrap();
    assert!(msg.contains("Connected to PM["));
    assert_eq!(c.pms.len(), 1);
    c.shutdown_all().await;
}

#[tokio::test]
async fn connect_same_path_reuses() {
    let mut c = Ctrl::new();
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().to_str().unwrap();
    c.connect(p).await.unwrap();
    let msg = c.connect(p).await.unwrap();
    assert!(msg.contains("Switched") && c.pms.len() == 1);
    c.shutdown_all().await;
}

#[tokio::test]
async fn connect_invalid_path_errors() {
    assert!(Ctrl::new().connect("/no_such_xyz_999").await.is_err());
}

#[tokio::test]
async fn connect_two_dirs() {
    let mut c = Ctrl::new();
    let (t1, t2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    c.connect(t1.path().to_str().unwrap()).await.unwrap();
    c.connect(t2.path().to_str().unwrap()).await.unwrap();
    assert_eq!(c.pms.len(), 2);
    c.shutdown_all().await;
}

// -- Ctrl::shutdown_all --
#[tokio::test]
async fn shutdown_all_completes() {
    let mut c = Ctrl::new();
    let (tx, mut rx) = mpsc::channel::<PmMsg>(16);
    let task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if matches!(msg, PmMsg::Shutdown) {
                break;
            }
        }
    });
    c.pms.insert(
        "/s".into(),
        PmHandle {
            name: "s".into(),
            project_path: "/s".into(),
            tx,
            task,
            status: Arc::new(Mutex::new("idle".to_string())),
            last_message: Arc::new(Mutex::new(String::new())),
        },
    );
    c.shutdown_all().await;
}

// -- PmHandle actor lifecycle --
#[tokio::test]
async fn actor_processes_task_and_shuts_down() {
    let (tx, rx) = mpsc::channel::<PmMsg>(16);
    let task = tokio::spawn(async move {
        let mut rx = rx;
        while let Some(msg) = rx.recv().await {
            match msg {
                PmMsg::Task { text, reply } => {
                    let _ = reply.send(Ok(format!("echo:{text}")));
                }
                PmMsg::Shutdown => break,
            }
        }
    });
    let (rtx, rrx) = oneshot::channel();
    tx.send(PmMsg::Task {
        text: "ping".into(),
        reply: rtx,
    })
    .await
    .unwrap();
    assert_eq!(rrx.await.unwrap().unwrap(), "echo:ping");
    tx.send(PmMsg::Shutdown).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

// -- Prompt transitions --
#[tokio::test]
async fn prompt_transitions() {
    let mut c = Ctrl::new();
    assert_eq!(c.prompt(), "CTRL> ");
    let tmp = tempfile::tempdir().unwrap();
    c.connect(tmp.path().to_str().unwrap()).await.unwrap();
    assert!(c.prompt().starts_with("PM[") && c.prompt().ends_with("]> "));
    c.disconnect();
    assert_eq!(c.prompt(), "CTRL> ");
    c.shutdown_all().await;
}
