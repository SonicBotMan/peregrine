//! Daemon identity payload — the `/health` body, shared by `pg ping` and the
//! future MCP bridge (one type, not one per shell).

use serde::{Deserialize, Serialize};

/// What a healthy daemon says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthInfo {
    pub name: String,
    pub version: String,
    pub pid: u32,
    pub uptime_secs: u64,
    pub status: String,
}

impl HealthInfo {
    /// Build the payload for a healthy daemon process.
    pub fn ok(pid: u32, uptime_secs: u64) -> Self {
        Self {
            name: crate::DAEMON_NAME.to_string(),
            version: crate::VERSION.to_string(),
            pid,
            uptime_secs,
            status: "ok".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_payload_roundtrips() {
        let info = HealthInfo::ok(42, 7);
        assert_eq!(info.name, "peregrine");
        assert_eq!(info.status, "ok");
        assert_eq!(info.pid, 42);
        let json = serde_json::to_string(&info).unwrap();
        let back: HealthInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
    }
}
