#[cfg(not(target_os = "macos"))]
use active_win_pos_rs::get_active_window;
use base64::{engine::general_purpose, Engine as _};
use chrono::Local;
use crossbeam_channel::unbounded;
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

fn get_active_window_info() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut ctx = AppInfoContext::new();
        ctx.refresh_apps().ok()?;
        ctx.get_frontmost_application().ok().map(|app| app.name)
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
                            // Check for Minecraft-specific arguments
                            if cmd
                                .iter()
                                .any(|arg| arg.to_str().map_or(false, |s| s.contains("minecraft")))
                            {
                                "Minecraft".to_string()
                            } else {
                                // Try to get the JAR file name
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

fn log(message: &str) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let log_message = format!("[{}] {}", timestamp, message);
    println!("{}", log_message);
    if let Err(e) = write_to_log_file(&log_message) {
        eprintln!("Failed to write to log file: {}", e);
    }
}

fn get_or_set_api_key() -> String {
    let user_dirs = UserDirs::new().expect("Failed to get user directories");
    let documents_dir = user_dirs
        .document_dir()
        .expect("Failed to get documents directory");
    let timelens_dir = documents_dir.join("timelens");
    let token_file = timelens_dir.join("token.txt");

    fs::create_dir_all(&timelens_dir).expect("Failed to create timelens directory");

    if let Ok(api_key) = fs::read_to_string(&token_file) {
        api_key.trim().to_string()
    } else {
        let api_key: String = Input::new()
            .with_prompt("Please enter your TimeLens API key")
            .interact_text()
            .expect("Failed to get user input");

        File::create(&token_file)
            .and_then(|mut file| file.write_all(api_key.as_bytes()))
            .expect("Failed to save API key");

        api_key
    }
}

pub fn run() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        if let Err(e) = run_internal().await {
            log(&format!("Error: {}", e));
        }
    });
}

async fn run_internal() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = get_or_set_api_key();
    let url = url::Url::parse("wss://timelens.wireway.ch/v2/event")?;

    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    let (_event_sender, event_receiver) = unbounded::<()>();

    tokio::spawn(async move {
        while event_receiver.recv().is_ok() {
            tx.send(()).await.ok();
        }
    });

    tokio::select! {
        _ = rx.recv() => {
            println!("Quitting application");
            return Ok(());
        }
        _ = async {
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

            match connect_async(request).await {
                Ok((ws_stream, _)) => {
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
                                            break;
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
                                        break;
                                    }
                                    None => {
                                        log("WebSocket connection closed");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => log(&format!("WebSocket connection error: {:?}", e)),
            }

            log("Connection closed, attempting to reconnect...");
            tokio::time::sleep(Duration::from_secs(5)).await;
        } => {}
    }

    Ok(())
}