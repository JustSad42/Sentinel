//! What each helper process of a browser or Electron app does, decoded from its command line.
//! Chromium (Chrome, Edge, Brave, Opera, WebView2, Electron apps such as Discord, Claude, VS Code)
//! and Firefox mark every child with its job; Task Manager just shows the same name 30 times.

/// A short label such as "Tab / page", "GPU process" or "Extension", or None for ordinary processes.
pub fn role(cmdline: &str) -> Option<String> {
    let c = cmdline;
    if let Some(t) = flag(c, "--type=") {
        let r = match t {
            "renderer" => {
                if c.contains("--extension-process") {
                    "Extension".into()
                } else if c.contains("--pdf-renderer") {
                    "PDF viewer".into()
                } else {
                    "Tab / page".into()
                }
            }
            "gpu-process" => "GPU process".into(),
            "utility" => match flag(c, "--utility-sub-type=") {
                Some(s) => utility(s),
                None => "Utility".into(),
            },
            "crashpad-handler" => "Crash reporter".into(),
            "broker" => "Broker".into(),
            "ppapi" | "ppapi-broker" => "Plugin".into(),
            "zygote" => "Zygote".into(),
            other => other.replace('-', " "),
        };
        return Some(r);
    }
    if c.contains(" -contentproc") || c.contains("\" -contentproc") {
        // Firefox: the process kind is the last argument.
        let kind = c.split_whitespace().last().unwrap_or("");
        let r = match kind {
            "tab" => "Tab / page",
            "gpu" => "GPU process",
            "socket" => "Network",
            "rdd" => "Media decoder",
            "utility" => "Utility",
            "gmplugin" => "Media plugin",
            "forkserver" => "Fork server",
            "vr" => "VR",
            _ => "Helper",
        };
        return Some(r.into());
    }
    None
}

/// The main process of a multi-process browser or Electron app (no --type flag, but its children have one).
pub fn is_main(cmdline: &str) -> bool {
    !cmdline.contains("--type=") && !cmdline.contains("-contentproc")
}

fn flag<'a>(c: &'a str, name: &str) -> Option<&'a str> {
    let i = c.find(name)? + name.len();
    let rest = &c[i..];
    let rest = rest.trim_start_matches('"');
    let end = rest.find(|ch: char| ch.is_whitespace() || ch == '"').unwrap_or(rest.len());
    Some(&rest[..end])
}

fn utility(sub: &str) -> String {
    let s = sub.to_ascii_lowercase();
    let known = [
        ("network.mojom", "Network service"),
        ("storage.mojom", "Storage service"),
        ("audio.mojom", "Audio service"),
        ("video_capture", "Camera service"),
        ("data_decoder", "Data decoder"),
        ("unzip", "Unzip service"),
        ("proxy_resolver", "Proxy resolver"),
        ("printing", "Printing service"),
        ("on_device_model", "On-device AI"),
        ("media_foundation", "Media decoder"),
        ("cdm", "DRM / protected media"),
        ("speech", "Speech service"),
        ("chrome.mojom.utilwin", "Windows utility"),
        ("tracing", "Tracing"),
        ("quarantine", "Download scanner"),
        ("shape_detection", "Shape detection"),
    ];
    for (k, v) in known {
        if s.contains(k) {
            return v.into();
        }
    }
    // e.g. "foo.mojom.BarService" -> "Bar service"
    let last = sub.rsplit('.').next().unwrap_or(sub);
    let mut out = String::new();
    for (i, ch) in last.chars().enumerate() {
        if i > 0 && ch.is_uppercase() {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    if out.is_empty() { "Utility".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chromium_roles() {
        assert_eq!(role(r#""C:\chrome.exe" --type=renderer --lang=en --renderer-client-id=7"#).as_deref(), Some("Tab / page"));
        assert_eq!(role(r#""C:\chrome.exe" --type=renderer --extension-process --lang=en"#).as_deref(), Some("Extension"));
        assert_eq!(role(r#""C:\chrome.exe" --type=gpu-process --gpu-preferences=x"#).as_deref(), Some("GPU process"));
        assert_eq!(role(r#""C:\chrome.exe" --type=utility --utility-sub-type=network.mojom.NetworkService --lang=en"#).as_deref(), Some("Network service"));
        assert_eq!(role(r#""C:\chrome.exe" --type=utility --utility-sub-type=foo.mojom.WidgetThing"#).as_deref(), Some("Widget thing"));
        assert_eq!(role(r#""C:\chrome.exe" --type=crashpad-handler"#).as_deref(), Some("Crash reporter"));
        assert_eq!(role(r#""C:\firefox.exe" -contentproc -childID 3 -isForBrowser 11 tab"#).as_deref(), Some("Tab / page"));
        assert_eq!(role(r#""C:\chrome.exe" --profile-directory=Default"#), None);
    }
}
