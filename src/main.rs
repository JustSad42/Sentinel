#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]

mod actions;
mod admin;
mod app;
mod store;
mod apphist;
mod apps;
mod boot;
mod config;
mod disk;
mod fmt;
mod graph;
mod hw;
mod icons;
mod info;
mod install;
mod integration;
mod net;
mod overlay;
mod palette;
mod pdh;
mod report;
mod roles;
mod sampler;
mod services;
mod startup;
mod theme;
mod ui_apps;
mod ui_misc;
mod ui_net;
mod ui_perf;
mod ui_procs;
mod updater;
mod win;

fn app_icon() -> egui::IconData {
    let img = image::load_from_memory(include_bytes!("../assets/icon.png")).expect("icon").to_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData { rgba: img.into_raw(), width, height }
}

fn main() -> eframe::Result<()> {
    let t0 = std::time::Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let has = |f: &str| args.iter().any(|a| a.eq_ignore_ascii_case(f));

    // Elevated one-shot helper: do the job and exit, no UI.
    if let Some(i) = args.iter().position(|a| a == "--admin-op") {
        if let (Some(hex), Some(out)) = (args.get(i + 1), args.get(i + 2)) {
            admin::helper_main(hex, out);
        }
        return Ok(());
    }

    // Diagnostics: print the hardware/kernel metrics (debug builds have a console).
    if has("--hw-dump") {
        hw::dump();
        return Ok(());
    }

    // Single instance: a second launch (Start menu, IFEO via Ctrl+Shift+Esc / taskmgr) just shows the first.
    if let Some(i) = args.iter().position(|a| a == "--locks") {
        if let Some(p) = args.get(i + 1) {
            integration::request_locks(p);
        }
    }
    let mut instance = integration::acquire(has("--takeover"));
    // A launch right after sign-in that nobody asked for (app restore, the Task Manager redirect)
    // must not pop the window up: treat it like --background.
    let quiet = !has("--show") && !has("--takeover") && !has("--locks") && integration::sign_in_launch();
    integration::log_launch(&format!(
        "args={:?} elevated={} running={} sign_in={} idle_ms={} quiet={quiet}",
        &args[1..],
        win::is_elevated(),
        instance.is_none(),
        integration::secs_since_sign_in().map(|s| format!("{s:.0}s")).unwrap_or("?".into()),
        integration::idle_ms()
    ));
    if instance.is_none() {
        if quiet {
            return Ok(());
        }
        if integration::signal_existing() {
            return Ok(());
        }
    }

    // Sentinel always runs as administrator. Hand off to the elevated launch task (no UAC prompt),
    // or on the very first run ask once; the elevated copy then registers the task.
    if !win::is_elevated() && !integration::is_dev_build() && !has("--no-elevate") {
        drop(instance.take());
        if !has("--background") && !quiet {
            integration::request_show_on_start();
        }
        if admin::run_task() {
            return Ok(());
        }
        let mut pass: Vec<String> = args[1..].iter().filter(|a| *a != "--takeover").cloned().collect();
        pass.push("--takeover".into());
        if win::shell_open(&integration::exe_path(), &pass.join(" "), "runas").is_ok() {
            return Ok(());
        }
        // UAC declined: run with limited rights rather than not at all.
        instance = integration::acquire(true);
    }

    // Elevated copies run only from Program Files: install there first, then carry on from it.
    if win::is_elevated() && !integration::is_dev_build() {
        match install::ensure() {
            Ok(Some(installed)) => {
                drop(instance.take());
                let mut cmd = std::process::Command::new(&installed);
                cmd.args(args[1..].iter().filter(|a| *a != "--takeover")).arg("--takeover");
                match cmd.spawn() {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        integration::log_launch(&format!("couldn't start the installed copy: {e}"));
                        instance = integration::acquire(true);
                    }
                }
            }
            Ok(None) => {}
            Err(e) => integration::log_launch(&format!("couldn't install to Program Files: {e}")),
        }
    }

    hw::warm();
    let cfg = config::Config::load();
    // Keep the Explorer menu pointing at this copy (it moves on reinstall/update).
    if cfg.explorer_menu && !integration::is_dev_build() && win::is_elevated() {
        let _ = integration::set_explorer_menu(true);
    }
    let start_hidden = (has("--background") || quiet) && !has("--show") && !integration::take_show_on_start();
    let size = cfg.window_size.unwrap_or([1320.0, 820.0]);
    let title = if config::test_mode() { "Sentinel (TEST COPY)" } else { "Sentinel" };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([size[0].max(980.0), size[1].max(620.0)])
            .with_min_inner_size([980.0, 620.0])
            .with_icon(app_icon())
            .with_visible(!start_hidden)
            .with_drag_and_drop(true)
            .with_window_level(if cfg.always_on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal }),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "Sentinel",
        options,
        Box::new(move |cc| {
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            Ok(Box::new(app::App::new(cc, instance, start_hidden, ms)))
        }),
    )
}
