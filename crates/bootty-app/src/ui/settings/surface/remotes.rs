use std::{
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use bootty_config::config::{SshAuthenticationConfig, SshHostKeyPolicyConfig, SshProfileConfig};
use eframe::egui;

use super::{
    SettingsSurface, section, settings_button, settings_notice, settings_segmented,
    settings_text_edit,
};

pub(super) type RemoteTest = (SshProfileConfig, mpsc::Sender<Result<(), String>>);

#[derive(Default)]
pub(super) struct EditorState {
    selected_id: Option<String>,
    draft: Option<ProfileDraft>,
    message: Option<(bool, String)>,
    test: Option<mpsc::Receiver<Result<(), String>>>,
    test_request: Option<SshProfileConfig>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ProfileDraft {
    name: String,
    host: String,
    user: String,
    port: String,
    authentication: SshAuthenticationConfig,
    host_key_policy: SshHostKeyPolicyConfig,
    identity_file: String,
    proxy_jump: String,
    program: String,
    args: Vec<String>,
}

impl ProfileDraft {
    fn from_profile(profile: &SshProfileConfig) -> Self {
        Self {
            name: profile.name.clone(),
            host: profile.host.clone(),
            user: profile.user.clone().unwrap_or_default(),
            port: profile
                .port
                .map(|port| port.to_string())
                .unwrap_or_default(),
            authentication: profile.authentication,
            host_key_policy: profile.host_key_policy,
            identity_file: profile
                .identity_file
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            proxy_jump: profile.proxy_jump.clone().unwrap_or_default(),
            program: profile.program.clone(),
            args: profile.args.clone(),
        }
    }

    fn new() -> Self {
        Self {
            name: "New Remote".to_owned(),
            program: "ssh".to_owned(),
            ..Self::default()
        }
    }

    fn profile(&self) -> Result<SshProfileConfig, String> {
        let name = self.name.trim();
        let host = self.host.trim();
        if name.is_empty() || host.is_empty() {
            return Err("Profile name and host name are required.".to_owned());
        }
        let port = super::nonempty(&self.port)
            .map(|port| {
                port.parse::<u16>()
                    .map_err(|_| "Port must be between 1 and 65535.".to_owned())
            })
            .transpose()?;
        let identity_file = super::nonempty(&self.identity_file).map(Into::into);
        if self.authentication != SshAuthenticationConfig::Auto && identity_file.is_none() {
            let credential = if self.authentication == SshAuthenticationConfig::Agent {
                "public key file that identifies the SSH-agent key"
            } else {
                "private key file"
            };
            return Err(format!("Choose a {credential}."));
        }
        Ok(SshProfileConfig {
            name: name.to_owned(),
            host: host.to_owned(),
            user: super::nonempty(&self.user),
            port,
            authentication: self.authentication,
            host_key_policy: self.host_key_policy,
            identity_file,
            proxy_jump: super::nonempty(&self.proxy_jump),
            program: super::nonempty(&self.program).unwrap_or_else(|| "ssh".to_owned()),
            args: self.args.clone(),
        })
    }
}

pub(super) fn ui(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let mut editor = std::mem::take(&mut win.remote_editor);
    editor.show(win, ui);
    win.remote_editor = editor;
}

impl EditorState {
    fn show(&mut self, win: &mut SettingsSurface, ui: &mut egui::Ui) {
        self.poll_test();
        self.ensure_selection(win);

        section(ui, win.palette, "SSH PROFILES");
        ui.horizontal_wrapped(|ui| {
            let mut selected = None;
            for (id, profile) in &win.config.ssh_profiles {
                if ui
                    .selectable_label(
                        self.selected_id.as_deref() == Some(id.as_str()),
                        &profile.name,
                    )
                    .clicked()
                {
                    selected = Some(id.clone());
                }
            }
            if let Some(id) = selected {
                self.select(win, id);
            }
            if settings_button(ui, win.palette, "Add Remote").clicked() {
                let id = new_profile_id();
                self.selected_id = Some(id);
                self.draft = Some(ProfileDraft::new());
                self.message = None;
                self.test = None;
            }
        });

        let Some(id) = self.selected_id.clone() else {
            settings_notice(
                ui,
                win.palette.muted,
                "Add an SSH profile to connect a Space to another machine.",
            );
            return;
        };
        let Some(mut draft) = self.draft.take() else {
            return;
        };

        section(ui, win.palette, "CONNECTION");
        field(ui, win, "Profile name", "Home Lab", &mut draft.name);
        field(
            ui,
            win,
            "Host name",
            "host or SSH config alias",
            &mut draft.host,
        );
        field(ui, win, "Port", "22 or from SSH config", &mut draft.port);
        field(
            ui,
            win,
            "User name",
            "local user or SSH config",
            &mut draft.user,
        );

        super::settings_row(
            ui,
            win.palette,
            "Authentication",
            "Use SSH config, the active SSH agent, or one private key file.",
            |ui| {
                let labels = ["SSH config", "SSH agent", "Private key"];
                let selected = match draft.authentication {
                    SshAuthenticationConfig::Auto => 0,
                    SshAuthenticationConfig::Agent => 1,
                    SshAuthenticationConfig::KeyFile => 2,
                };
                if let Some(index) = settings_segmented(ui, win.palette, &labels, selected) {
                    draft.authentication = match index {
                        1 => SshAuthenticationConfig::Agent,
                        2 => SshAuthenticationConfig::KeyFile,
                        _ => SshAuthenticationConfig::Auto,
                    };
                }
            },
        );
        super::settings_row(
            ui,
            win.palette,
            "Host trust",
            "Strict rejects unknown hosts. Trust new records a first-seen host key in OpenSSH.",
            |ui| {
                let labels = ["Strict", "Trust new host"];
                let selected = match draft.host_key_policy {
                    SshHostKeyPolicyConfig::Strict => 0,
                    SshHostKeyPolicyConfig::AcceptNew => 1,
                };
                if let Some(index) = settings_segmented(ui, win.palette, &labels, selected) {
                    draft.host_key_policy = if index == 1 {
                        SshHostKeyPolicyConfig::AcceptNew
                    } else {
                        SshHostKeyPolicyConfig::Strict
                    };
                }
            },
        );
        if draft.authentication != SshAuthenticationConfig::Auto {
            let (label, hint) = if draft.authentication == SshAuthenticationConfig::Agent {
                ("Agent public key file", "/Users/name/.ssh/id_ed25519.pub")
            } else {
                ("Private key file", "/Users/name/.ssh/id_ed25519")
            };
            field(ui, win, label, hint, &mut draft.identity_file);
        }
        field(
            ui,
            win,
            "Proxy / jump host",
            "optional host or SSH config alias",
            &mut draft.proxy_jump,
        );
        field(ui, win, "SSH client", "ssh", &mut draft.program);

        if let Some((success, message)) = &self.message {
            settings_notice(
                ui,
                if *success {
                    win.palette.success
                } else {
                    win.palette.destructive
                },
                message,
            );
        }
        if self.test.is_some() {
            settings_notice(ui, win.palette.muted, "Testing SSH connection…");
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }

        let mut deleted = false;
        ui.horizontal(|ui| {
            if settings_button(ui, win.palette, "Test Connection").clicked() {
                match draft.profile() {
                    Ok(profile) => {
                        self.test_request = Some(profile);
                        self.message = None;
                    }
                    Err(error) => self.message = Some((false, error)),
                }
            }
            if settings_button(ui, win.palette, "Save").clicked() {
                match draft.profile() {
                    Ok(profile) => {
                        save_profile(win, &id, profile);
                        self.message = Some((true, "Profile saved.".to_owned()));
                    }
                    Err(error) => self.message = Some((false, error)),
                }
            }
            if win.config.ssh_profiles.contains_key(&id)
                && settings_button(ui, win.palette, "Delete").clicked()
            {
                delete_profile(win, &id);
                self.selected_id = None;
                self.draft = None;
                self.message = None;
                self.test = None;
                deleted = true;
            }
        });
        if !deleted {
            self.draft = Some(draft);
        }
    }

    fn ensure_selection(&mut self, win: &SettingsSurface) {
        if self
            .selected_id
            .as_ref()
            .is_some_and(|id| self.draft.is_some() || win.config.ssh_profiles.contains_key(id))
        {
            return;
        }
        if let Some((id, profile)) = win.config.ssh_profiles.first_key_value() {
            self.selected_id = Some(id.clone());
            self.draft = Some(ProfileDraft::from_profile(profile));
        }
    }

    fn select(&mut self, win: &SettingsSurface, id: String) {
        self.draft = win
            .config
            .ssh_profiles
            .get(&id)
            .map(ProfileDraft::from_profile);
        self.selected_id = Some(id);
        self.message = None;
        self.test = None;
    }

    pub(super) fn take_test(&mut self) -> Option<RemoteTest> {
        let request = self.test_request.take()?;
        let (sender, receiver) = mpsc::channel();
        self.test = Some(receiver);
        Some((request, sender))
    }

    fn poll_test(&mut self) {
        let Some(receiver) = &self.test else {
            return;
        };
        let Ok(result) = receiver.try_recv() else {
            return;
        };
        self.test = None;
        self.message = Some(match result {
            Ok(()) => (true, "SSH connection succeeded.".to_owned()),
            Err(error) => (false, error),
        });
    }
}

fn field(
    ui: &mut egui::Ui,
    win: &mut SettingsSurface,
    label: &str,
    hint: &str,
    value: &mut String,
) {
    super::settings_row(ui, win.palette, label, "", |ui| {
        settings_text_edit(ui, win.palette, value, hint);
    });
}

fn save_profile(win: &mut SettingsSurface, id: &str, profile: SshProfileConfig) {
    win.config
        .ssh_profiles
        .insert(id.to_owned(), profile.clone());
    let id = id.to_owned();
    win.writeback
        .mutate(move |document| document.set_ssh_profile(&id, &profile));
}

fn delete_profile(win: &mut SettingsSurface, id: &str) {
    win.config.ssh_profiles.remove(id);
    let id = id.to_owned();
    win.writeback
        .mutate(move |document| document.remove_ssh_profile(&id));
}

fn new_profile_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("remote-{nanos:x}")
}
