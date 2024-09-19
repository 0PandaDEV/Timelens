#[cfg(not(target_os = "macos"))]
use active_win_pos_rs::get_active_window;
use base64::{engine::general_purpose, Engine as _};
use chrono::Local;
use dialoguer::Input;
use directories::UserDirs;
use futures_util::{SinkExt, StreamExt};
use http::{header, Request};
use rand::Rng;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(not(target_os = "macos"))]
use std::path::Path;
use std::time::Duration;
#[cfg(not(target_os = "macos"))]
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use std::path::PathBuf;

#[cfg(target_os = "macos")]
use applications::{AppInfoContext, AppInfo};

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        if let Err(e) = run_internal().await {
            log(&format!("Error: {}", e));
            Err(e)
        } else {
            Ok(())
        }
    })
}

fn get_active_window_info() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut ctx = AppInfoContext::new();
        ctx.refresh_apps().ok()?;
        ctx.get_frontmost_application().ok().and_then(|app| {
            if app.name != "loginwindow" {
                Some(app.name)
            } else {
                None
            }
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        let mut system = System::new_all();
        system.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new());

        get_active_window().ok().and_then(|window| {
            let pid = sysinfo::Pid::from_u32(window.process_id as u32);
            system
                .process(pid)
                .map(|process| {
                    let cmd = process.cmd();
                    if !cmd.is_empty() {
                        if cmd[0].to_str().map_or(false, |s| s.contains("java")) {
                            if cmd
                                .iter()
                                .any(|arg| arg.to_str().map_or(false, |s| s.contains("minecraft")))
                            {
                                "Minecraft".to_string()
                            } else {
                                cmd.iter()
                                    .position(|arg| arg == "-jar")
                                    .and_then(|pos| cmd.get(pos + 1))
                                    .and_then(|jar_path| {
                                        Path::new(jar_path)
                                            .file_stem()
                                            .and_then(|name| name.to_str())
                                            .map(String::from)
                                    })
                                    .unwrap_or_else(|| "Unknown Java App".to_string())
                            }
                        } else {
                            window.app_name.clone()
                        }
                    } else {
                        window.app_name.clone()
                    }
                })
                .or_else(|| Some(window.app_name.clone()))
                .filter(|name| !name.is_empty())
                .or_else(|| Some("Unknown App".to_string()))
        })
    }
}

fn get_log_file_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let user_dirs = UserDirs::new().ok_or("Failed to get user directories")?;
    let mut path = user_dirs.document_dir().ok_or("Failed to get document directory")?.to_path_buf();
    path.push("timelens");
    path.push("timelens.log");
    Ok(path)
}

fn write_to_log_file(message: &str) -> Result<(), Box<dyn std::error::Error>> {
    let log_file = get_log_file_path()?;
    let timelens_dir = log_file.parent().ok_or("Invalid log file path")?;

    fs::create_dir_all(timelens_dir)?;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)?;

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let log_entry = format!("[{}] {}\n", timestamp, message);

    file.write_all(log_entry.as_bytes())?;
    Ok(())
}

pub fn log(message: &str) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let log_message = format!("[{}] {}", timestamp, message);
    println!("{}", log_message);
    if let Err(e) = write_to_log_file(&log_message) {
        eprintln!("Failed to write to log file: {}", e);
    }
}

fn get_or_set_api_key() -> Result<String, Box<dyn std::error::Error>> {
    let user_dirs = UserDirs::new().ok_or("Failed to get user directories")?;
    let documents_dir = user_dirs
        .document_dir()
        .ok_or("Failed to get documents directory")?;
    let timelens_dir = documents_dir.join("timelens");
    let token_file = timelens_dir.join("token.txt");

    fs::create_dir_all(&timelens_dir)?;

    match fs::read_to_string(&token_file) {
        Ok(api_key) => Ok(api_key.trim().to_string()),
        Err(_) => {
            let api_key: String = Input::new()
                .with_prompt("Please enter your TimeLens API key")
                .interact_text()?;

            fs::write(&token_file, api_key.as_bytes())?;
            Ok(api_key)
        }
    }
}

async fn run_internal() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = get_or_set_api_key()?;
    let url = url::Url::parse("wss://timelens.wireway.ch/v2/event")?;

    loop {
        match connect_and_run(&api_key, &url).await {
            Ok(_) => {
                log("WebSocket connection closed normally. Reconnecting...");
            }
            Err(e) => {
                log(&format!("Error in WebSocket connection: {:?}. Reconnecting...", e));
            }
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn connect_and_run(api_key: &str, url: &url::Url) -> Result<(), Box<dyn std::error::Error>> {
    let mut rng = rand::thread_rng();
    let key: [u8; 16] = rng.gen();
    let key = general_purpose::STANDARD.encode(&key);

    let request = Request::builder()
        .uri(url.as_str())
        .header("Host", url.host_str().unwrap_or("timelens.wireway.ch"))
        .header("Authorization", format!("Bearer {}", api_key))
        .header(header::UPGRADE, "websocket")
        .header(header::CONNECTION, "Upgrade")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, key)
        .body(())
        .unwrap();

    let (ws_stream, _) = connect_async(request).await?;
    log("WebSocket connection established");
    let (mut write, mut read) = ws_stream.split();

    let mut last_window_info = String::new();

    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if let Some(app_name) = get_active_window_info() {
                    log(&format!("{}", app_name));

                    if app_name != last_window_info {
                        last_window_info = app_name.clone();

                        if let Err(e) = write.send(Message::Text(app_name)).await {
                            log(&format!("Error sending active window info: {:?}", e));
                            return Err(e.into());
                        }
                    }
                } else {
                    log("Failed to get active window info");
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(_)) => {},
                    Some(Err(e)) => {
                        log(&format!("Error receiving message: {:?}", e));
                        return Err(e.into());
                    }
                    None => {
                        log("WebSocket connection closed");
                        return Ok(());
                    }
                }
            }
        }
    }
}