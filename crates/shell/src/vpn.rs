//! Mullvad status, read from the system daemon via the `mullvad` CLI.
//! Placeholder for the `tunnel` crate (P3), which will talk to the daemon's management socket.

use std::{
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub enum VpnStatus {
    #[default]
    Unknown,
    Missing,
    Disconnected,
    Connecting,
    Connected { relay: String, location: String, ipv4: String },
}

impl VpnStatus {
    fn parse(output: &str) -> Self {
        let mut lines = output.lines();
        let state = lines.next().unwrap_or_default().trim();
        let relay = || {
            output
                .lines()
                .find_map(|l| l.trim().strip_prefix("Relay:"))
                .map(|r| r.split_whitespace().next().unwrap_or_default().to_owned())
                .unwrap_or_default()
        };
        // `Visible location:       Bulgaria, Sofia. IPv4: 1.2.3.4`
        let visible = output.lines().find_map(|l| l.trim().strip_prefix("Visible location:")).unwrap_or_default();
        let (location, ipv4) = match visible.split_once("IPv4:") {
            Some((loc, ip)) => (loc.trim().trim_end_matches('.').to_owned(), ip.split_whitespace().next().unwrap_or_default().to_owned()),
            None => (visible.trim().trim_end_matches('.').to_owned(), String::new()),
        };
        match state {
            s if s.starts_with("Connected") => Self::Connected { relay: relay(), location, ipv4 },
            s if s.starts_with("Connecting") || s.starts_with("Disconnecting") => Self::Connecting,
            s if s.starts_with("Disconnected") || s.starts_with("Blocked") => Self::Disconnected,
            _ => Self::Unknown,
        }
    }
}

/// Polls the daemon on a background thread; calls `on_change` whenever the status changes.
pub fn watch(on_change: impl Fn() + Send + 'static) -> Arc<Mutex<VpnStatus>> {
    let status = Arc::new(Mutex::new(VpnStatus::Unknown));
    let shared = status.clone();
    thread::Builder::new()
        .name("vpn-status".into())
        .spawn(move || loop {
            let next = match crate::console_tool("mullvad").args(["status", "-v"]).output() {
                Ok(out) => VpnStatus::parse(&String::from_utf8_lossy(&out.stdout)),
                Err(_) => VpnStatus::Missing,
            };
            let changed = {
                let mut current = shared.lock().unwrap();
                let changed = *current != next;
                *current = next;
                changed
            };
            if changed {
                on_change();
            }
            thread::sleep(Duration::from_secs(3));
        })
        .expect("spawn vpn-status thread");
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_connected_relay() {
        let out = "Connected\n    Relay:                  bg-sof-wg-001 (1.2.3.4:48596/UDP)\n    Features:   Quantum Resistance\n    Visible location:       Bulgaria, Sofia. IPv4: 185.1.2.3\n";
        assert_eq!(
            VpnStatus::parse(out),
            VpnStatus::Connected { relay: "bg-sof-wg-001".into(), location: "Bulgaria, Sofia".into(), ipv4: "185.1.2.3".into() }
        );
    }

    #[test]
    fn parses_disconnected() {
        assert_eq!(VpnStatus::parse("Disconnected\n"), VpnStatus::Disconnected);
    }
}
