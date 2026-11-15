use rusterisk_api::{AsteriskInfo, BuildInfo, ConfigInfo, SetId, StatusInfo, SystemInfo};

#[derive(Clone)]
pub(crate) struct ServerInfo {
    entity_id: uuid::Uuid,
    startup: String,
    capacity: i32,
}

impl ServerInfo {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entity_id: uuid::Uuid::new_v4(),
            startup: chrono::Utc::now().to_rfc3339(),
            capacity: capacity.min(64) as i32,
        }
    }

    pub(crate) fn selected(&self, only: Option<&str>) -> AsteriskInfo {
        let selected = |name: &str| {
            only.is_none_or(|only| {
                only.split(',')
                    .any(|value| value.eq_ignore_ascii_case(name))
            })
        };
        AsteriskInfo {
            build: selected("build").then(|| BuildInfo {
                os: std::env::consts::OS.into(),
                kernel: String::new(),
                options: String::new(),
                machine: std::env::consts::ARCH.into(),
                date: String::new(),
                user: String::new(),
            }),
            system: selected("system").then(|| SystemInfo {
                version: env!("CARGO_PKG_VERSION").into(),
                entity_id: self.entity_id.to_string(),
            }),
            config: selected("config").then(|| ConfigInfo {
                name: "rusterisk".into(),
                default_language: "en".into(),
                max_channels: Some(self.capacity),
                max_open_files: None,
                max_load: None,
                setid: SetId {
                    user: String::new(),
                    group: String::new(),
                },
            }),
            status: selected("status").then(|| StatusInfo {
                startup_time: self.startup.clone(),
                last_reload_time: self.startup.clone(),
            }),
        }
    }
}
