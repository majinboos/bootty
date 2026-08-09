use std::{
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use bootty_config::{
    config::{SshAuthenticationConfig, SshHostKeyPolicyConfig, SshProfileConfig},
    toml_edit,
};
use eframe::egui;

use super::{
    SettingsWindow, section, settings_button, settings_notice, settings_segmented,
    settings_text_edit,
};

#[derive(Default)]
pub(super) struct EditorState {
    selected_id: Option<String>,
    draft: Option<ProfileDraft>,
    message: Option<(bool, String)>,
    test: ConnectionTest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
            host: String::new(),
            user: String::new(),
            port: String::new(),
            authentication: SshAuthenticationConfig::Auto,
            host_key_policy: SshHostKeyPolicyConfig::Strict,
            identity_file: String::new(),
            proxy_jump: String::new(),
            program: "ssh".to_owned(),
            args: Vec::new(),
        }
    }

    fn profile(&self) -> Result<SshProfileConfig, String> {
        let name = self.name.trim();
        let host = self.host.trim();
        if name.is_empty() || host.is_empty() {
            return Err("Profile name and host name are required.".to_owned());
        }
        let port = if self.port.trim().is_empty() {
            None
        } else {
            Some(
                self.port
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| "Port must be between 1 and 65535.".to_owned())?,
            )
        };
        let identity_file = nonempty(&self.identity_file).map(Into::into);
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
            user: nonempty(&self.user),
            port,
            authentication: self.authentication,
            host_key_policy: self.host_key_policy,
            identity_file,
            proxy_jump: nonempty(&self.proxy_jump),
            program: nonempty(&self.program).unwrap_or_else(|| "ssh".to_owned()),
            args: self.args.clone(),
        })
    }
}

#[derive(Default)]
enum ConnectionTest {
    #[default]
    Idle,
    Running(mpsc::Receiver<Result<(), String>>),
    Passed,
    Failed(String),
}

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let mut editor = std::mem::take(&mut win.remote_editor);
    editor.show(win, ui);
    win.remote_editor = editor;
}

impl EditorState {
    fn show(&mut self, win: &mut SettingsWindow, ui: &mut egui::Ui) {
        self.poll_test();
        self.ensure_selection(win);

        section(ui, win.palette, "SSH PROFILES");
        ui.horizontal_wrapped(|ui| {
            let profiles = win
                .config
                .ssh_profiles
                .iter()
                .map(|(id, profile)| (id.clone(), profile.name.clone()))
                .collect::<Vec<_>>();
            for (id, name) in profiles {
                if ui
                    .selectable_label(self.selected_id.as_deref() == Some(id.as_str()), name)
                    .clicked()
                {
                    self.select(win, id);
                }
            }
            if settings_button(ui, win.palette, "Add Remote").clicked() {
                let id = new_profile_id();
                self.selected_id = Some(id);
                self.draft = Some(ProfileDraft::new());
                self.message = None;
                self.test = ConnectionTest::Idle;
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
        match &self.test {
            ConnectionTest::Idle => {}
            ConnectionTest::Running(_) => {
                settings_notice(ui, win.palette.muted, "Testing SSH connection…");
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(50));
            }
            ConnectionTest::Passed => {
                settings_notice(ui, win.palette.success, "SSH connection succeeded.");
            }
            ConnectionTest::Failed(error) => {
                settings_notice(ui, win.palette.destructive, error);
            }
        }

        let mut deleted = false;
        ui.horizontal(|ui| {
            if settings_button(ui, win.palette, "Test Connection").clicked() {
                match draft.profile() {
                    Ok(profile) => self.start_test(ui.ctx(), profile),
                    Err(error) => self.test = ConnectionTest::Failed(error),
                }
            }
            if settings_button(ui, win.palette, "Save").clicked() {
                match draft.profile() {
                    Ok(profile) => {
                        save_profile(win, &id, &profile);
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
                self.test = ConnectionTest::Idle;
                deleted = true;
            }
        });
        if !deleted {
            self.draft = Some(draft);
        }
    }

    fn ensure_selection(&mut self, win: &SettingsWindow) {
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

    fn select(&mut self, win: &SettingsWindow, id: String) {
        self.draft = win
            .config
            .ssh_profiles
            .get(&id)
            .map(ProfileDraft::from_profile);
        self.selected_id = Some(id);
        self.message = None;
        self.test = ConnectionTest::Idle;
    }

    fn start_test(&mut self, ctx: &egui::Context, profile: SshProfileConfig) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = crate::remote_catalog::list_remote(&profile)
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        self.test = ConnectionTest::Running(receiver);
    }

    fn poll_test(&mut self) {
        let ConnectionTest::Running(receiver) = &self.test else {
            return;
        };
        let Ok(result) = receiver.try_recv() else {
            return;
        };
        self.test = match result {
            Ok(()) => ConnectionTest::Passed,
            Err(error) => ConnectionTest::Failed(error),
        };
    }
}

fn field(ui: &mut egui::Ui, win: &mut SettingsWindow, label: &str, hint: &str, value: &mut String) {
    super::settings_row(ui, win.palette, label, "", |ui| {
        settings_text_edit(ui, win.palette, value, hint);
    });
}

fn save_profile(win: &mut SettingsWindow, id: &str, profile: &SshProfileConfig) {
    win.config
        .ssh_profiles
        .insert(id.to_owned(), profile.clone());
    let id = id.to_owned();
    let profile = profile.clone();
    win.write(move |document| {
        let root = ["ssh-profiles", id.as_str()];
        document.remove_item(&root)?;
        document.set_item(&[root[0], root[1], "name"], toml_edit::value(&profile.name))?;
        document.set_item(&[root[0], root[1], "host"], toml_edit::value(&profile.host))?;
        if let Some(user) = &profile.user {
            document.set_item(&[root[0], root[1], "user"], toml_edit::value(user))?;
        }
        if let Some(port) = profile.port {
            document.set_item(
                &[root[0], root[1], "port"],
                toml_edit::value(i64::from(port)),
            )?;
        }
        let authentication = match profile.authentication {
            SshAuthenticationConfig::Auto => "auto",
            SshAuthenticationConfig::Agent => "agent",
            SshAuthenticationConfig::KeyFile => "key-file",
        };
        document.set_item(
            &[root[0], root[1], "authentication"],
            toml_edit::value(authentication),
        )?;
        let host_key_policy = match profile.host_key_policy {
            SshHostKeyPolicyConfig::Strict => "strict",
            SshHostKeyPolicyConfig::AcceptNew => "accept-new",
        };
        document.set_item(
            &[root[0], root[1], "host-key-policy"],
            toml_edit::value(host_key_policy),
        )?;
        if let Some(identity_file) = &profile.identity_file {
            document.set_item(
                &[root[0], root[1], "identity-file"],
                toml_edit::value(identity_file.display().to_string()),
            )?;
        }
        if let Some(proxy_jump) = &profile.proxy_jump {
            document.set_item(
                &[root[0], root[1], "proxy-jump"],
                toml_edit::value(proxy_jump),
            )?;
        }
        document.set_item(
            &[root[0], root[1], "program"],
            toml_edit::value(&profile.program),
        )?;
        if !profile.args.is_empty() {
            let mut args = toml_edit::Array::new();
            for arg in &profile.args {
                args.push(arg.as_str());
            }
            document.set_item(&[root[0], root[1], "args"], toml_edit::value(args))?;
        }
        Ok(())
    });
}

fn delete_profile(win: &mut SettingsWindow, id: &str) {
    win.config.ssh_profiles.remove(id);
    let id = id.to_owned();
    win.write(move |document| document.remove_item(&["ssh-profiles", id.as_str()]));
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn new_profile_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("remote-{nanos:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_round_trips_structured_profile_fields() {
        let profile = SshProfileConfig {
            name: "Home Lab".to_owned(),
            host: "nas".to_owned(),
            user: Some("admin".to_owned()),
            port: Some(2222),
            authentication: SshAuthenticationConfig::KeyFile,
            host_key_policy: SshHostKeyPolicyConfig::AcceptNew,
            identity_file: Some("/tmp/key".into()),
            proxy_jump: Some("gateway".to_owned()),
            program: "ssh".to_owned(),
            args: Vec::new(),
        };

        assert_eq!(ProfileDraft::from_profile(&profile).profile(), Ok(profile));
    }

    #[test]
    fn saved_profile_round_trips_through_the_config_document() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let config = crate::config::BoottyConfig {
            config_path: path.clone(),
            ..crate::config::BoottyConfig::default()
        };
        let mut win = SettingsWindow::new(config);
        let profile = SshProfileConfig {
            name: "Local Mac".to_owned(),
            host: "localhost".to_owned(),
            user: Some("luan".to_owned()),
            port: Some(22),
            authentication: SshAuthenticationConfig::Agent,
            host_key_policy: SshHostKeyPolicyConfig::Strict,
            identity_file: Some("/Users/luan/.ssh/id_ed25519.pub".into()),
            proxy_jump: None,
            program: "ssh".to_owned(),
            args: Vec::new(),
        };

        save_profile(&mut win, "local-mac", &profile);

        let loaded = crate::config::load_config_from_path(path).expect("load saved profile");
        assert_eq!(loaded.ssh_profiles["local-mac"], profile);
    }

    #[test]
    fn key_file_draft_requires_a_path() {
        let draft = ProfileDraft {
            authentication: SshAuthenticationConfig::KeyFile,
            host: "host".to_owned(),
            ..ProfileDraft::new()
        };

        assert!(draft.profile().unwrap_err().contains("private key"));
    }
}
