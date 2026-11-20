//! "Export system report": a Markdown snapshot of the hardware, Windows, what's running, startup
//! apps, services and installed software, for troubleshooting or sharing.

use crate::app::{App, Msg, ToastKind};
use crate::fmt;
use std::fmt::Write;

pub fn export(app: &mut App) {
    let snap = app.snap.clone();
    // Names for the process table, resolved now (the info cache lives on the UI side).
    let top: Vec<(String, f32, u64, u32)> = {
        let mut by: std::collections::HashMap<String, (String, f32, u64, u32)> = std::collections::HashMap::new();
        for p in &snap.procs {
            let name = app.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.description.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| p.name.clone());
            let e = by.entry(p.name.to_lowercase()).or_insert((name, 0.0, 0, 0));
            e.1 += p.cpu;
            e.2 += p.mem;
            e.3 += 1;
        }
        let mut top: Vec<(String, f32, u64, u32)> = by.into_values().collect();
        top.sort_by(|a, b| b.2.cmp(&a.2));
        top.truncate(25);
        top
    };
    let impact = crate::boot::load();
    let tx = app.tx.clone();
    let day = {
        let (y, m, d, ..) = fmt::civil(crate::win::now_unix());
        format!("{y}-{m:02}-{d:02}")
    };
    std::thread::spawn(move || {
        let Some(path) = rfd::FileDialog::new().set_title("Save system report").set_file_name(format!("Sentinel report {day}.md")).add_filter("Markdown", &["md"]).add_filter("Text", &["txt"]).save_file() else {
            return;
        };
        let text = build(&snap, &top, &impact);
        let msg = match std::fs::write(&path, text) {
            Ok(()) => {
                crate::win::reveal_in_explorer(&path.to_string_lossy());
                Msg::Toast(ToastKind::Good, "System report saved".into(), None)
            }
            Err(e) => Msg::Toast(ToastKind::Bad, format!("Couldn't save the report: {e}"), None),
        };
        let _ = tx.send(msg);
    });
}

fn build(snap: &crate::sampler::Snapshot, top: &[(String, f32, u64, u32)], boot: &crate::boot::BootRecord) -> String {
    let mut o = String::new();
    let st = crate::hw::statics().cloned().unwrap_or_default();
    let now = crate::win::now_unix();
    let _ = writeln!(o, "# System report\n\nCreated by Sentinel {} on {} {}.\n", crate::updater::VERSION, fmt::when(now), fmt::clock(now));

    let _ = writeln!(o, "## System\n");
    let rows = [
        ("Windows", format!("{} (build {})", st.sys.os, st.sys.build)),
        ("Computer", format!("{} {}", st.sys.maker, st.sys.model).trim().to_string()),
        ("Motherboard", st.sys.board.clone()),
        ("BIOS", format!("{} ({})", st.sys.bios, st.sys.bios_date)),
        ("Up time", fmt::duration(crate::win::uptime_secs())),
    ];
    table(&mut o, &["", ""], rows.iter().filter(|r| !r.1.trim().is_empty()).map(|(k, v)| vec![k.to_string(), v.clone()]).collect());

    let _ = writeln!(o, "\n## Hardware\n");
    let c = &st.cpu;
    let mut hw = vec![
        vec!["Processor".into(), format!("{} ({} cores, {} threads)", cpu_name(), c.cores, c.logical)],
        vec!["Caches".into(), format!("L1 {} · L2 {} · L3 {}", fmt::bytes(c.l1), fmt::bytes(c.l2), fmt::bytes(c.l3))],
        vec!["Memory".into(), format!("{} {} {}", fmt::bytes(if st.mem.installed > 0 { st.mem.installed } else { snap.mem_total }), st.mem.kind(), if st.mem.speed() > 0 { format!("{} MT/s", st.mem.speed()) } else { String::new() })],
    ];
    for d in &st.mem.dimms {
        hw.push(vec![format!("  {}", d.slot), format!("{} {} {} MT/s · {} {}", fmt::bytes(d.size), d.kind, d.configured.max(d.speed), d.maker, d.part)]);
    }
    let (gname, vram) = crate::ui_perf::gpu_info();
    hw.push(vec!["Graphics".into(), format!("{gname} ({}) · driver {} ({})", fmt::bytes(*vram), st.sys.gpu_driver, st.sys.gpu_driver_date)]);
    for d in &snap.disks {
        if let Some(i) = &d.info {
            let vols: Vec<String> = d.volumes.iter().map(|v| format!("{}: {} free of {}", v.letter, fmt::bytes(v.free), fmt::bytes(v.total))).collect();
            hw.push(vec![format!("Disk {}", d.name), format!("{} · {} {} · {}{} · {}", i.model, i.bus, match i.ssd { Some(true) => "SSD", Some(false) => "HDD", None => "" }, fmt::bytes(i.size), d.temperature.map(|t| format!(" · {t} °C")).unwrap_or_default(), vols.join(", "))]);
        }
    }
    for a in &snap.adapters {
        hw.push(vec![format!("Network: {}", a.alias), format!("{} · {} · {}", a.description, fmt::link_speed(a.speed), a.ipv4.join(", "))]);
    }
    table(&mut o, &["", ""], hw);

    let _ = writeln!(o, "\n## Right now\n");
    let used = snap.mem_total.saturating_sub(snap.mem_avail);
    let x = &snap.extra;
    let mut now_rows = vec![
        vec!["CPU".into(), format!("{}{}", fmt::pct(snap.cpu_total), x.cpu_mhz.map(|m| format!(" at {}", fmt::mhz(m))).unwrap_or_default())],
        vec!["Memory".into(), format!("{} of {} in use · committed {} of {}", fmt::bytes(used), fmt::bytes(snap.mem_total), fmt::bytes(snap.commit_total), fmt::bytes(snap.commit_limit))],
        vec!["GPU".into(), format!("{}{}", fmt::pct(snap.gpu.util), x.gpu.temperature.map(|t| format!(" · {t:.0} °C")).unwrap_or_default())],
        vec!["Processes".into(), format!("{} processes · {} threads · {} handles", snap.proc_count, snap.thread_count, snap.handle_count)],
    ];
    if let Some(t) = &x.tcp {
        now_rows.push(vec!["TCP".into(), format!("{} established · {:.2}% retransmitted", t.established, t.retrans_pct)]);
    }
    table(&mut o, &["", ""], now_rows);

    let _ = writeln!(o, "\n## Top apps by memory\n");
    table(&mut o, &["App", "Processes", "CPU", "Memory"], top.iter().map(|(n, c, m, k)| vec![n.clone(), k.to_string(), fmt::pct(*c), fmt::bytes(*m)]).collect());

    let _ = writeln!(o, "\n## Startup apps\n");
    let items = crate::startup::list();
    table(
        &mut o,
        &["App", "Enabled", "Impact (last boot)", "Command"],
        items
            .iter()
            .map(|i| {
                let imp = boot.impact_for(&i.target).map(|x| x.label()).unwrap_or("Not measured");
                vec![i.description.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| i.name.clone()), if i.enabled { "Yes".into() } else { "No".into() }, imp.into(), format!("`{}`", i.command.replace('|', "\\|"))]
            })
            .collect(),
    );
    if boot.boot_unix > 0 {
        let _ = writeln!(o, "\nLast boot: desktop ready {} after power-on{}.", boot.desktop_secs.map(|s| format!("{s:.0} s")).unwrap_or("?".into()), boot.settled_secs.map(|s| format!(", startup apps settled after {s:.0} s")).unwrap_or_default());
    }

    let _ = writeln!(o, "\n## Services set to start automatically but not running\n");
    let svcs = crate::services::list_full();
    let stopped: Vec<Vec<String>> = svcs.iter().filter(|s| s.start_type == 2 && !s.running() && !s.delayed).map(|s| vec![s.display.clone(), s.name.clone()]).collect();
    if stopped.is_empty() {
        let _ = writeln!(o, "None.");
    } else {
        table(&mut o, &["Service", "Name"], stopped);
    }
    let running = svcs.iter().filter(|s| s.running()).count();
    let _ = writeln!(o, "\n{} of {} services running.", running, svcs.len());

    let _ = writeln!(o, "\n## Installed apps\n");
    let mut apps = crate::apps::list();
    apps.sort_by_key(|a| a.name.to_lowercase());
    table(&mut o, &["App", "Version", "Publisher", "Size"], apps.iter().map(|a| vec![a.name.clone(), a.version.clone(), a.publisher.clone(), if a.size_kb > 0 { fmt::bytes(a.size_kb * 1024) } else { String::new() }]).collect());
    o
}

fn cpu_name() -> String {
    winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0")
        .and_then(|k| k.get_value::<String, _>("ProcessorNameString"))
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

fn table(o: &mut String, head: &[&str], rows: Vec<Vec<String>>) {
    let esc = |s: &str| s.replace('|', "\\|").replace('\n', " ");
    let _ = writeln!(o, "| {} |", head.iter().map(|h| esc(h)).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(o, "|{}|", head.iter().map(|_| "---").collect::<Vec<_>>().join("|"));
    for r in rows {
        let _ = writeln!(o, "| {} |", r.iter().map(|c| if c.starts_with('`') { c.clone() } else { esc(c) }).collect::<Vec<_>>().join(" | "));
    }
}
