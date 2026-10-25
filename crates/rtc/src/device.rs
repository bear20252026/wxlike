//! G09-03 设备注册表与降级策略。
//!
//! 真实采集（麦克风/扬声器/摄像头/屏幕/蓝牙）由 G10/G12 原生桥提供；
//! 本模块管的是**选择与失效语义**，这层最容易出错：
//!
//! * 权限被拒绝时不得留下一个仍在占用的采集会话；
//! * 设备拔出时先停采集，再尝试同类型回退，不把"还在采集"报给上层；
//! * 选择不存在的设备明确失败，不静默切到默认设备。
//!
//! 采集本身通过 [`crate::capture`] 的 feature 模块；这里的状态机可以在
//! 无硬件的 CI 里逐条验证。

use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Microphone,
    Speaker,
    Camera,
    Screen,
}

impl DeviceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::Speaker => "speaker",
            Self::Camera => "camera",
            Self::Screen => "screen",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct MediaDevice {
    pub id: String,
    pub kind: DeviceKind,
    pub label: String,
    pub available: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Prompt,
    Granted,
    Denied,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DeviceError {
    #[error("no such device: {0}")]
    Unknown(String),
    #[error("device {0} is not available")]
    Unavailable(String),
    #[error("device {0} is not a {1}")]
    WrongKind(String, &'static str),
    #[error("capture permission denied")]
    PermissionDenied,
    #[error("no {0} selected")]
    NoSelection(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceEvent {
    Selected {
        kind: DeviceKind,
        id: String,
    },
    Unplugged {
        kind: DeviceKind,
        id: String,
        fallback: Option<String>,
    },
    CaptureStarted {
        kind: DeviceKind,
        id: String,
    },
    CaptureStopped {
        kind: DeviceKind,
        reason: &'static str,
    },
    PermissionGranted,
    /// 权限被拒时返回被强制停止的采集路数（麦克风/摄像头）。
    PermissionDenied {
        stopped: usize,
    },
}

/// 选择 + 采集状态。一个实例代表一个客户端进程。
pub struct DeviceRegistry {
    devices: BTreeMap<String, MediaDevice>,
    selected: HashMap<DeviceKind, String>,
    capturing: HashMap<DeviceKind, String>,
    permission: Permission,
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceRegistry {
    pub fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
            selected: HashMap::new(),
            capturing: HashMap::new(),
            permission: Permission::Prompt,
        }
    }

    pub fn permission(&self) -> Permission {
        self.permission
    }

    pub fn grant_permission(&mut self) -> DeviceEvent {
        self.permission = Permission::Granted;
        DeviceEvent::PermissionGranted
    }

    /// 权限被拒：立刻停掉所有采集，不能继续占着麦克风。
    pub fn deny_permission(&mut self) -> DeviceEvent {
        self.permission = Permission::Denied;
        let stopped = self.capturing.len();
        self.capturing.clear();
        DeviceEvent::PermissionDenied { stopped }
    }

    pub fn add(&mut self, id: &str, kind: DeviceKind, label: &str) {
        self.devices.insert(
            id.to_string(),
            MediaDevice {
                id: id.to_string(),
                kind,
                label: label.to_string(),
                available: true,
            },
        );
    }

    pub fn list(&self, kind: DeviceKind) -> Vec<&MediaDevice> {
        self.devices.values().filter(|d| d.kind == kind).collect()
    }

    pub fn selected(&self, kind: DeviceKind) -> Option<&str> {
        self.selected.get(&kind).map(String::as_str)
    }

    pub fn capturing(&self, kind: DeviceKind) -> Option<&str> {
        self.capturing.get(&kind).map(String::as_str)
    }

    /// 选择设备：必须存在、可用、类型匹配，且采集权限未被拒绝。
    pub fn select(&mut self, kind: DeviceKind, id: &str) -> Result<DeviceEvent, DeviceError> {
        if self.permission == Permission::Denied {
            return Err(DeviceError::PermissionDenied);
        }
        let device = self
            .devices
            .get(id)
            .ok_or_else(|| DeviceError::Unknown(id.into()))?;
        if device.kind != kind {
            return Err(DeviceError::WrongKind(id.into(), kind.label()));
        }
        if !device.available {
            return Err(DeviceError::Unavailable(id.into()));
        }
        self.selected.insert(kind, id.to_string());
        Ok(DeviceEvent::Selected {
            kind,
            id: id.to_string(),
        })
    }

    /// 开始采集。未授权时返回 `PermissionDenied` 且不建立采集。
    pub fn start_capture(&mut self, kind: DeviceKind) -> Result<DeviceEvent, DeviceError> {
        if self.permission != Permission::Granted {
            return Err(DeviceError::PermissionDenied);
        }
        let id = self
            .selected
            .get(&kind)
            .cloned()
            .ok_or(DeviceError::NoSelection(kind.label()))?;
        let device = self
            .devices
            .get(&id)
            .ok_or_else(|| DeviceError::Unknown(id.clone()))?;
        if !device.available {
            return Err(DeviceError::Unavailable(id));
        }
        self.capturing.insert(kind, id.clone());
        Ok(DeviceEvent::CaptureStarted { kind, id })
    }

    pub fn stop_capture(&mut self, kind: DeviceKind) {
        self.capturing.remove(&kind);
    }

    /// 设备拔出：若正在用，先停采集，再尝试同类型回退（回退不自动开采集）。
    pub fn unplug(&mut self, id: &str) -> Option<DeviceEvent> {
        let device = self.devices.get_mut(id)?;
        device.available = false;
        let kind = device.kind;
        let was_selected = self.selected.get(&kind).is_some_and(|s| s == id);
        if !was_selected {
            return None;
        }
        self.selected.remove(&kind);
        self.capturing.remove(&kind);
        let fallback = self
            .devices
            .values()
            .find(|d| d.kind == kind && d.available)
            .map(|d| d.id.clone());
        if let Some(next) = &fallback {
            self.selected.insert(kind, next.clone());
        }
        Some(DeviceEvent::Unplugged {
            kind,
            id: id.to_string(),
            fallback,
        })
    }

    /// 热插拔：设备重新出现（蓝牙重连等）。不自动选择。
    pub fn replug(&mut self, id: &str) -> bool {
        match self.devices.get_mut(id) {
            Some(d) => {
                d.available = true;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> DeviceRegistry {
        let mut r = DeviceRegistry::new();
        r.add("mic-1", DeviceKind::Microphone, "Built-in mic");
        r.add("cam-1", DeviceKind::Camera, "Front camera");
        r.add("cam-2", DeviceKind::Camera, "Back camera");
        r.add("bt-mic", DeviceKind::Microphone, "Bluetooth headset");
        r
    }

    #[test]
    fn denied_permission_stops_capture_and_never_starts_it() {
        let mut r = registry();
        r.grant_permission();
        r.select(DeviceKind::Microphone, "mic-1").unwrap();
        r.start_capture(DeviceKind::Microphone).unwrap();
        assert_eq!(r.capturing(DeviceKind::Microphone), Some("mic-1"));

        let ev = r.deny_permission();
        assert_eq!(ev, DeviceEvent::PermissionDenied { stopped: 1 });
        assert_eq!(r.capturing(DeviceKind::Microphone), None);
        assert_eq!(
            r.start_capture(DeviceKind::Microphone),
            Err(DeviceError::PermissionDenied)
        );
        assert_eq!(
            r.select(DeviceKind::Microphone, "mic-1"),
            Err(DeviceError::PermissionDenied)
        );
        assert!(
            r.capturing(DeviceKind::Microphone).is_none(),
            "不持续占麦克风"
        );
    }

    #[test]
    fn unplug_stops_capture_and_picks_a_fallback() {
        let mut r = registry();
        r.grant_permission();
        r.select(DeviceKind::Camera, "cam-1").unwrap();
        r.start_capture(DeviceKind::Camera).unwrap();

        let ev = r.unplug("cam-1").unwrap();
        assert_eq!(
            ev,
            DeviceEvent::Unplugged {
                kind: DeviceKind::Camera,
                id: "cam-1".into(),
                fallback: Some("cam-2".into()),
            }
        );
        assert_eq!(r.selected(DeviceKind::Camera), Some("cam-2"));
        assert_eq!(r.capturing(DeviceKind::Camera), None, "不自动续采");
        assert_eq!(
            r.start_capture(DeviceKind::Camera).unwrap(),
            DeviceEvent::CaptureStarted {
                kind: DeviceKind::Camera,
                id: "cam-2".into()
            }
        );
    }

    #[test]
    fn unplug_without_fallback_reports_none() {
        let mut r = DeviceRegistry::new();
        r.add("cam-1", DeviceKind::Camera, "Only camera");
        r.grant_permission();
        r.select(DeviceKind::Camera, "cam-1").unwrap();
        let ev = r.unplug("cam-1").unwrap();
        assert!(matches!(ev, DeviceEvent::Unplugged { fallback: None, .. }));
        assert_eq!(r.selected(DeviceKind::Camera), None);
        assert_eq!(
            r.start_capture(DeviceKind::Camera),
            Err(DeviceError::NoSelection("camera"))
        );
    }

    #[test]
    fn selection_rejects_unknown_unavailable_and_wrong_kind() {
        let mut r = registry();
        assert!(matches!(
            r.select(DeviceKind::Camera, "nope"),
            Err(DeviceError::Unknown(_))
        ));
        assert!(matches!(
            r.select(DeviceKind::Microphone, "cam-1"),
            Err(DeviceError::WrongKind(_, "microphone"))
        ));
        r.grant_permission();
        r.unplug("bt-mic");
        assert_eq!(
            r.select(DeviceKind::Microphone, "bt-mic"),
            Err(DeviceError::Unavailable("bt-mic".into()))
        );
        assert!(r.replug("bt-mic"));
        r.select(DeviceKind::Microphone, "bt-mic").unwrap();
    }
}
