use mystery_novel_agent::config::AppConfig;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

#[test]
fn proxy_setting_defaults_off_and_round_trips() {
  assert!(!AppConfig::default().use_system_proxy);
  let old: AppConfig = toml::from_str("[llm]\n").unwrap();
  assert!(!old.use_system_proxy);
  for enabled in [false, true] {
    let cfg = AppConfig {
      use_system_proxy: enabled,
      ..AppConfig::default()
    };
    let saved = toml::to_string(&cfg).unwrap();
    let loaded: AppConfig = toml::from_str(&saved).unwrap();
    assert_eq!(loaded.use_system_proxy, enabled);
  }
}

#[test]
fn proxy_setting_controls_environment_proxy() {
  // 子进程隔离代理环境变量，避免 Rust 2024 下修改共享环境或污染其他测试。
  for enabled in [false, true] {
    let output = Command::new(std::env::current_exe().unwrap())
      .args(["--exact", "proxy_setting_child", "--nocapture"])
      .env("MYSTERY_TEST_PROXY_ENABLED", enabled.to_string())
      .output()
      .unwrap();
    assert!(
      output.status.success(),
      "proxy={enabled}: {}\n{}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr)
    );
  }
}

#[test]
fn proxy_setting_child() {
  let Ok(enabled) = std::env::var("MYSTERY_TEST_PROXY_ENABLED") else {
    return;
  };
  let enabled = enabled == "true";
  let origin = TcpListener::bind("127.0.0.1:0").unwrap();
  let target = format!("http://{}/cover.jpg", origin.local_addr().unwrap());
  let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
  let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
  // 目标与代理返回不同正文，验证实际路由，无需连接外网。
  for (listener, body) in [(origin, "direct"), (proxy, "proxy")] {
    std::thread::spawn(move || {
      let (mut socket, _) = listener.accept().unwrap();
      socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
      let mut request = [0; 4096];
      socket.read(&mut request).unwrap();
      write!(
        socket,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
      )
      .unwrap();
    });
  }
  // 这里仍通过独立子进程设置环境，当前进程不修改环境。
  let output = Command::new(std::env::current_exe().unwrap())
    .args(["--exact", "proxy_request_child", "--nocapture"])
    .env("MYSTERY_TEST_PROXY_TARGET", target)
    .env("MYSTERY_TEST_PROXY_ENABLED", enabled.to_string())
    .env("HTTP_PROXY", &proxy_url)
    .env("http_proxy", &proxy_url)
    .env("HTTPS_PROXY", &proxy_url)
    .env("https_proxy", &proxy_url)
    .env("ALL_PROXY", &proxy_url)
    .env("all_proxy", &proxy_url)
    .env("NO_PROXY", "")
    .env("no_proxy", "")
    .output()
    .unwrap();
  assert!(
    output.status.success(),
    "{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
}

#[tokio::test]
async fn proxy_request_child() {
  let Ok(target) = std::env::var("MYSTERY_TEST_PROXY_TARGET") else {
    return;
  };
  let enabled = std::env::var("MYSTERY_TEST_PROXY_ENABLED").unwrap() == "true";
  let cfg = AppConfig {
    use_system_proxy: enabled,
    ..AppConfig::default()
  };
  let response = cfg
    .http_client_builder()
    .timeout(Duration::from_secs(5))
    .build()
    .unwrap()
    .get(target)
    .send()
    .await
    .unwrap()
    .text()
    .await
    .unwrap();
  assert_eq!(response, if enabled { "proxy" } else { "direct" });
}
