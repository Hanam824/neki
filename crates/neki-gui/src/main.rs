//! Neki Desktop App: a Slint front-end over `neki-core`.

// No console window behind the app in Windows release builds (no-op elsewhere).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod registry;

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    app::install(&ui);
    ui.run()
}
