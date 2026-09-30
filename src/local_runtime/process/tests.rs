use super::*;
use std::io::{Read, Write};

const MODE: &str = "BABEL_TEST_PROVIDER_PROCESS_MODE";

#[test]
#[ignore = "private local runtime child process fixture"]
fn helper_process() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    if mode == "oversized" {
        print!("{}", "x".repeat(8193));
        std::io::stdout().flush().unwrap();
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let announcement = serde_json::json!({
        "service":"babel-whisper", "port":port, "pid":std::process::id(),
        "endpoint":format!("http://127.0.0.1:{port}/inference")
    });
    println!("BABEL_SERVICE_READY {announcement}");
    std::io::stdout().flush().unwrap();
    if mode == "exit" {
        return;
    }
    for connection in listener.incoming() {
        let Ok(mut stream) = connection else {
            break;
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut input = [0; 2048];
        let _ = stream.read(&mut input);
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    }
}

fn command(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "local_runtime::process::tests::helper_process",
            "--nocapture",
        ])
        .env(MODE, mode);
    command
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap()
}
async fn wait_closed(endpoint: &str) {
    for _ in 0..50 {
        if client().get(endpoint).send().await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("owned helper remained reachable after its owner stopped");
}

#[tokio::test]
async fn independent_children_bind_their_own_ports_and_stop_only_the_owner() {
    let mut first = ServiceProcess::start(command("normal"), "babel-whisper", "/inference")
        .await
        .unwrap();
    let mut second = ServiceProcess::start(command("normal"), "babel-whisper", "/inference")
        .await
        .unwrap();
    assert_ne!(first.endpoint, second.endpoint);
    assert!(first.healthy() && second.healthy());
    let first_endpoint = first.endpoint.clone();
    let second_endpoint = second.endpoint.clone();
    assert_eq!(
        client()
            .get(&first_endpoint)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "ok"
    );
    first.stop().await;
    wait_closed(&first_endpoint).await;
    assert!(second.healthy());
    assert_eq!(
        client()
            .get(&second_endpoint)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "ok"
    );
    drop(second);
    wait_closed(&second_endpoint).await;
}

#[tokio::test]
async fn invalid_or_dead_children_are_not_accepted_as_ready() {
    let oversized =
        ServiceProcess::start(command("oversized"), "babel-whisper", "/inference").await;
    assert!(oversized.err().unwrap().to_string().contains("limit"));
    if let Ok(mut child) =
        ServiceProcess::start(command("exit"), "babel-whisper", "/inference").await
    {
        for _ in 0..50 {
            if !child.healthy() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("exited child was not detected");
    }
}

#[test]
fn readiness_rejects_unbound_remote_or_wrong_process_endpoints() {
    let valid = serde_json::json!({"service":"babel-whisper","port":49157,"pid":42,"endpoint":"http://127.0.0.1:49157/inference"});
    assert!(
        announced_endpoint(
            &serde_json::to_vec(&valid).unwrap(),
            "babel-whisper",
            "/inference",
            Some(42)
        )
        .is_ok()
    );
    for (field, value) in [
        ("service", serde_json::json!("different-service")),
        ("port", serde_json::json!(0)),
        ("pid", serde_json::json!(43)),
        (
            "endpoint",
            serde_json::json!("http://192.0.2.1:49157/inference"),
        ),
        (
            "endpoint",
            serde_json::json!("http://127.0.0.1:49158/inference"),
        ),
        (
            "endpoint",
            serde_json::json!("http://user:secret@127.0.0.1:49157/inference"),
        ),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = value;
        let error = announced_endpoint(
            &serde_json::to_vec(&invalid).unwrap(),
            "babel-whisper",
            "/inference",
            Some(42),
        )
        .unwrap_err()
        .to_string();
        assert!(!error.contains("secret"));
    }
}
