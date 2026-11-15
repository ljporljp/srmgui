use std::collections::HashMap;

use gpui_kit::{App, AppContext as _, Entity, SharedString};

use crate::config_io;
use crate::config_io::CustomSource;
use crate::model::{Probe, Status, ThemeChoice};

/// Source of truth for the whole UI.
#[derive(Debug, Clone)]
pub(crate) struct State {
    pub(crate) selected: SharedString,
    pub(crate) current: Option<SharedString>,
    pub(crate) active_tab: usize,
    pub(crate) customs: HashMap<String, CustomSource>,
    pub(crate) status: Status,
    pub(crate) probes: HashMap<String, Probe>,
    pub(crate) theme: ThemeChoice,
}

impl State {
    fn new(current: Option<String>, customs: HashMap<String, CustomSource>) -> Self {
        let selected = current.clone().unwrap_or_else(|| "rsproxy".to_string());
        Self {
            selected: selected.into(),
            current: current.map(Into::into),
            active_tab: 0,
            customs,
            status: Status::Info("选择一个源，然后点击「应用」写入 ~/.cargo/config.toml".into()),
            probes: HashMap::new(),
            theme: ThemeChoice::System,
        }
    }
}

/// Top-level application model.
pub(crate) struct CargoMirrorApp {
    pub(crate) state: State,
}

impl CargoMirrorApp {
    pub(crate) fn new(cx: &mut App) -> Entity<Self> {
        let current = config_io::current_source().ok().flatten();
        let customs = config_io::read_customs().unwrap_or_default();
        cx.new(|_cx| Self {
            state: State::new(current, customs),
        })
    }
}
