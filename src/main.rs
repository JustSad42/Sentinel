#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]

mod actions;
mod admin;
mod app;
mod apps;
mod config;
mod disk;
mod fmt;
mod graph;
mod hw;
mod icons;
mod info;
mod integration;
mod net;
mod pdh;
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
    let mut instance = integration::acquire(has("--takeover"));
    if instance.is_none() && integration::signal_existing() {
        return Ok(());
    }

    // Sentinel always runs as administrator. Hand off to the elevated launch task (no UAC prompt),
    // or on the very first run ask once; the elevated copy then registers the task.
    if !win::is_elevated() && !integration::is_dev_build() && !has("--no-elevate") {
        drop(instance.take());
        if !has("--background") {
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

    hw::warm();
    let cfg = config::Config::load();
    let start_hidden = has("--background") && !has("--show") && !integration::take_show_on_start();
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
