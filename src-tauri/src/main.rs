#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod timelens;
mod tray;

use tauri::Manager;
use tauri_plugin_autostart::{MacosLauncher, AutoLaunchManager};

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .setup(|app| {
            let window = app.get_webview_window("main").unwrap();
            window.hide().unwrap();

            std::thread::spawn(move || {
                timelens::run();
            });

            tray::setup(app)?;

            let auto_launch_manager = app.state::<AutoLaunchManager>();
            auto_launch_manager.enable().unwrap();

            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}