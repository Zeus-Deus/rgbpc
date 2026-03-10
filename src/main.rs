mod core;
mod ui;

use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 && args[1] == "--sync-theme" {
        if let Err(e) = core::perform_sync(false) {
            eprintln!("Sync failed: {}", e);
        }
        return Ok(());
    }

    if args.len() > 1 && args[1] == "--restore-last" {
        if let Err(e) = core::perform_restore() {
            eprintln!("Restore failed: {}", e);
        }
        return Ok(());
    }

    // Launch TUI
    let mut app = ui::app::App::new();
    app.run()?;

    Ok(())
}
