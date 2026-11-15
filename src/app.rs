use std::collections::HashMap;

use gpui_kit::{App, AppContext as _, Entity, SharedString};

use crate::config_io;
use crate::config_io::CustomSource;
use crate::model::{Language, Probe, Status, ThemeChoice};
use crate::update::UpdateState;

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
    pub(crate) language: Language,
    pub(crate) update: UpdateState,
}

impl State {
    fn new(
        current: Option<String>,
        customs: HashMap<String, CustomSource>,
        language: Language,
    ) -> Self {
        let selected = current.clone().unwrap_or_else(|| "rsproxy".to_string());
        Self {
            selected: selected.into(),
            current: current.map(Into::into),
            active_tab: 0,
            customs,
            status: Status::Info(rust_i18n::t!("status.hint").to_string().into()),
            probes: HashMap::new(),
            theme: ThemeChoice::System,
            language,
            update: UpdateState::Idle,
        }
    }
}

/// Top-level application model.
pub(crate) struct CargoMirrorApp {
    pub(crate) state: State,
}

impl CargoMirrorApp {
    pub(crate) fn new(cx: &mut App) -> Entity<Self> {
        // 先按持久化的语言设置初始化 locale，后续所有文案按该语言生成。
        let language = config_io::read_language()
            .map(|code| Language::from_code(&code))
            .unwrap_or_default();
        rust_i18n::set_locale(language.code());

        let current = config_io::current_source().ok().flatten();
        let customs = config_io::read_customs().unwrap_or_default();
        cx.new(|_cx| Self {
            state: State::new(current, customs, language),
        })
    }
}
