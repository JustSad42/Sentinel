//! Number and time formatting.

pub fn bytes(b: u64) -> String {
    let b = b as f64;
    if b < 1024.0 {
        format!("{b:.0} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        let m = b / 1024.0 / 1024.0;
        if m < 10.0 { format!("{m:.1} MB") } else { format!("{m:.0} MB") }
    } else if b < 1024.0 * 1024.0 * 1024.0 * 1000.0 {
        format!("{:.2} GB", b / 1024.0 / 1024.0 / 1024.0)
    } else {
        format!("{:.2} TB", b / 1024.0 / 1024.0 / 1024.0 / 1024.0)
    }
}

pub fn rate(bps: f64) -> String {
    if bps < 1.0 {
        "0 B/s".into()
    } else if bps < 1024.0 {
        format!("{bps:.0} B/s")
    } else if bps < 1024.0 * 1024.0 {
        format!("{:.0} KB/s", bps / 1024.0)
    } else if bps < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB/s", bps / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GB/s", bps / 1024.0 / 1024.0 / 1024.0)
    }
}

/// Network speeds are conventionally shown in bits.
pub fn bits(bps: f64) -> String {
    let b = bps * 8.0;
    if b < 1000.0 {
        format!("{b:.0} bps")
    } else if b < 1e6 {
        format!("{:.0} Kbps", b / 1e3)
    } else if b < 1e9 {
        format!("{:.1} Mbps", b / 1e6)
    } else {
        format!("{:.2} Gbps", b / 1e9)
    }
}

pub fn link_speed(bps: u64) -> String {
    let b = bps as f64;
    if b >= 1e9 { format!("{:.0} Gbps", b / 1e9) } else if b >= 1e6 { format!("{:.0} Mbps", b / 1e6) } else { format!("{:.0} Kbps", b / 1e3) }
}

/// Compact counts for per-second rates: 950, 54.8K, 1.2M.
pub fn count(v: f64) -> String {
    if v < 1000.0 {
        format!("{v:.0}")
    } else if v < 1e6 {
        if v < 1e4 { format!("{:.1}K", v / 1e3) } else { format!("{:.0}K", v / 1e3) }
    } else {
        format!("{:.1}M", v / 1e6)
    }
}

pub fn mhz(m: u32) -> String {
    if m >= 1000 { format!("{:.2} GHz", m as f32 / 1000.0) } else { format!("{m} MHz") }
}

pub fn pct(v: f32) -> String {
    if v < 0.05 { "0%".into() } else if v < 10.0 { format!("{v:.1}%") } else { format!("{v:.0}%") }
}

pub fn duration(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

pub fn cpu_time(t100ns: u64) -> String {
    let s = t100ns / 10_000_000;
    format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn local_offset_secs() -> i64 {
    use windows_sys::Win32::System::Time::*;
    unsafe {
        let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let r = GetTimeZoneInformation(&mut tz);
        let mut bias = tz.Bias;
        if r == 2 {
            bias += tz.DaylightBias;
        } else if r == 1 {
            bias += tz.StandardBias;
        }
        -(bias as i64) * 60
    }
}

/// (year, month, day, hour, minute, second) in local time.
fn civil(unix: i64) -> (i64, u32, u32, u32, u32, u32) {
    let t = unix + local_offset_secs();
    let days = t.div_euclid(86400);
    let rem = t.rem_euclid(86400);
    // Howard Hinnant's days-from-civil inverse
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32, (rem % 60) as u32)
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

pub fn clock(unix: i64) -> String {
    let (_, _, _, h, m, s) = civil(unix);
    format!("{h:02}:{m:02}:{s:02}")
}

/// "Today 14:03", "Yesterday 09:12", "Nov 3 18:40".
pub fn when(unix: i64) -> String {
    let now = crate::win::now_unix();
    let (y, mo, d, h, mi, _) = civil(unix);
    let (ny, nmo, nd, ..) = civil(now);
    let (yy, ymo, yd, ..) = civil(now - 86400);
    if (y, mo, d) == (ny, nmo, nd) {
        format!("Today {h:02}:{mi:02}")
    } else if (y, mo, d) == (yy, ymo, yd) {
        format!("Yesterday {h:02}:{mi:02}")
    } else if y == ny {
        format!("{} {d} {h:02}:{mi:02}", MONTHS[(mo - 1) as usize])
    } else {
        format!("{} {d}, {y}", MONTHS[(mo - 1) as usize])
    }
}

pub fn ago(unix: i64) -> String {
    let s = (crate::win::now_unix() - unix).max(0) as u64;
    if s < 60 { "just now".into() } else { format!("{} ago", duration(s).split(' ').next().unwrap_or("")) }
}

/// yyyymmdd (registry InstallDate) → "Mar 4, 2024".
pub fn install_date(s: &str) -> String {
    if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
        let y = &s[0..4];
        let m: usize = s[4..6].parse().unwrap_or(1);
        let d: u32 = s[6..8].parse().unwrap_or(1);
        format!("{} {d}, {y}", MONTHS[m.clamp(1, 12) - 1])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats() {
        assert_eq!(super::bytes(512), "512 B");
        assert_eq!(super::bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(super::rate(0.0), "0 B/s");
        assert_eq!(super::duration(3725), "1h 2m");
        assert_eq!(super::install_date("20240304"), "Mar 4, 2024");
    }
}
