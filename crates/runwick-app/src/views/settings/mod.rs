//! Settings section bodies (S13/S14). The page frame and nav live in `shell::settings_page`.
//! Each section is owned by one feature track in its own file.

pub mod automation;
pub mod backup;
pub mod collab;
pub mod devices;
pub mod env;
pub mod general;
pub mod mcp;
pub mod roles;
pub mod tools;

use crate::App;
use crate::state::SettingsSection;

#[derive(Default)]
pub struct State {
    pub general: general::State,
    pub tools: tools::State,
    pub env: env::State,
    pub mcp: mcp::State,
    pub collab: collab::State,
    pub roles: roles::State,
    pub automation: automation::State,
    pub devices: devices::State,
    pub backup: backup::State,
}

/// Body of a settings section, drawn by the settings page frame (`shell::settings_page`).
pub fn show_section(app: &mut App, ui: &mut egui::Ui, section: SettingsSection) {
    match section {
        SettingsSection::General => general::show(app, ui),
        SettingsSection::Tools => tools::show(app, ui),
        SettingsSection::Env => env::show(app, ui),
        SettingsSection::Mcp => mcp::show(app, ui),
        SettingsSection::Collab => collab::show(app, ui),
        SettingsSection::Roles => roles::show(app, ui),
        SettingsSection::Automation => automation::show(app, ui),
        SettingsSection::Devices => devices::show(app, ui),
        SettingsSection::Backup => backup::show(app, ui),
    }
}
