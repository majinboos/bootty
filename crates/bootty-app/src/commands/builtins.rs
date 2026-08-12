use super::{
    ArgumentSchema, CommandDescriptor, CommandExecutorResolver, CommandOrigin, CommandTargetKind,
    CompactSchema, MutationClass, ResourceKind, ValueType, validation::argument,
};

pub(super) fn core_descriptor(
    id: &str,
    title: &str,
    description: &str,
    mutation: MutationClass,
    arguments: CompactSchema,
    target: Option<ResourceKind>,
    palette: bool,
) -> CommandDescriptor {
    CommandDescriptor {
        id: id.to_owned(),
        title: title.to_owned(),
        description: description.to_owned(),
        aliases: Vec::new(),
        origin: Some(CommandOrigin::Core),
        mutation,
        arguments,
        result_schema: None,
        targets: public_catalog_target(id, target).into_iter().collect(),
        availability: None,
        target,
        palette,
        palette_metadata: None,
    }
}
fn public_catalog_target(id: &str, target: Option<ResourceKind>) -> Option<CommandTargetKind> {
    match id {
        "system.ping" | "system.describe" | "instance.describe" => {
            Some(CommandTargetKind::Instance)
        }
        "event.subscribe" | "event.snapshot" | "event.rebase" | "event.unsubscribe" => {
            Some(CommandTargetKind::Subscription)
        }
        "task.status" | "task.cancel" => Some(CommandTargetKind::Task),
        _ => target.map(catalog_target),
    }
}

fn catalog_target(target: ResourceKind) -> CommandTargetKind {
    match target {
        ResourceKind::Instance => CommandTargetKind::Instance,
        ResourceKind::ApplicationWindow => CommandTargetKind::ApplicationWindow,
        ResourceKind::Binding => CommandTargetKind::Binding,
        ResourceKind::Space => CommandTargetKind::Space,
        ResourceKind::Session => CommandTargetKind::Session,
        ResourceKind::MuxWindow => CommandTargetKind::Window,
        ResourceKind::Pane => CommandTargetKind::Pane,
        ResourceKind::Terminal => CommandTargetKind::Terminal,
        ResourceKind::Client => CommandTargetKind::Client,
        ResourceKind::Directory => CommandTargetKind::Directory,
        ResourceKind::Worktree => CommandTargetKind::Worktree,
        ResourceKind::Task => CommandTargetKind::Task,
        ResourceKind::Subscription => CommandTargetKind::Subscription,
        ResourceKind::Surface => CommandTargetKind::Surface,
        ResourceKind::Extension => CommandTargetKind::Extension,
    }
}

pub(super) fn mutation_for(id: &str) -> MutationClass {
    if matches!(
        id,
        "app.quit"
            | "app.window.close"
            | "pane.close"
            | "pane.kill"
            | "session.ditch"
            | "space.close"
            | "worktree.remove"
    ) {
        MutationClass::Destructive
    } else if id.ends_with(".describe")
        || id.ends_with(".list")
        || id.ends_with(".get")
        || id.ends_with(".status")
        || matches!(
            id,
            "system.ping"
                | "terminal.read"
                | "terminal.search"
                | "directory.resolve"
                | "event.snapshot"
        )
    {
        MutationClass::Read
    } else {
        MutationClass::Write
    }
}

pub(super) fn canonical_action_id(action: &str) -> Option<&'static str> {
    Some(match action {
        "toggle_fullscreen" => "app.fullscreen.toggle",
        "quit" => "app.quit",
        "close_window" => "app.window.close",
        "change_appearance" => "appearance.set",
        "copy_to_clipboard" => "clipboard.copy",
        "paste_from_clipboard" => "clipboard.paste",
        "reload_config" => "config.reload",
        "decrease_font_size" => "font.decrease",
        "increase_font_size" => "font.increase",
        "reset_font_size" => "font.reset",
        "set_font_size" => "font.set",
        "ignore" => "input.ignore",
        "close_surface" => "pane.close",
        "select_pane" => "pane.focus_direction",
        "kill_pane" => "pane.kill",
        "next_pane" => "pane.next",
        "previous_pane" => "pane.previous",
        "toggle_pane_zoom" => "pane.zoom",
        "ditch_session" => "session.ditch",
        "last_session" => "session.last",
        "move_session" => "session.move",
        "next_session" => "session.next",
        "session_picker" => "session.picker",
        "select_session" => "session.select",
        "previous_session" => "session.previous",
        "rename_session" => "session.rename",
        "close_space" => "space.close",
        "create_space" => "space.create",
        "edit_space" => "space.edit",
        "next_space" => "space.next",
        "previous_space" => "space.previous",
        "select_space" => "space.select",
        "copy_mode" => "terminal.copy_mode",
        "scroll_to_bottom" => "terminal.scroll.bottom",
        "scroll_page_lines" => "terminal.scroll.lines",
        "scroll_page_down" => "terminal.scroll.page_down",
        "scroll_page_up" => "terminal.scroll.page_up",
        "scroll_to_top" => "terminal.scroll.top",
        "search" => "terminal.search",
        "end_search" => "terminal.search.close",
        "navigate_search:next" => "terminal.search.next",
        "navigate_search:previous" => "terminal.search.previous",
        "search_selection" => "terminal.search.selection",
        "start_search" => "terminal.search.start",
        "csi" => "terminal.send_csi",
        "esc" => "terminal.send_esc",
        "text" => "terminal.send_text",
        "command_palette" => "ui.command_palette.open",
        "show_keybinds" => "ui.keybindings.open",
        "open_settings" => "ui.settings.open",
        "toggle_sidebar_focus" => "ui.sidebar.focus",
        "toggle_sidebar_visibility" => "ui.sidebar.toggle",
        "switch_theme" => "ui.theme_picker.open",
        "new_mux_session" => "session.create",
        "split_down" | "split_right" => "pane.split",
        "new_tab" => "window.create",
        "last_tab" => "window.last",
        "move_tab" => "window.move",
        "next_tab" => "window.next",
        "previous_tab" => "window.previous",
        "rename_tab" => "window.rename",
        "select_tab" => "window.select",
        _ => return None,
    })
}

pub(super) const fn compatibility_aliases() -> &'static [(&'static str, &'static str)] {
    &[
        ("ping", "system.ping"),
        ("list-commands", "command.list"),
        ("workspace.create", "space.create"),
        ("workspace.edit", "space.edit"),
        ("workspace.select", "space.select"),
        ("workspace.next", "space.next"),
        ("workspace.previous", "space.previous"),
        ("workspace.close", "space.close"),
        ("new-session", "session.create"),
        ("rename-session", "session.rename"),
        ("new-window", "window.create"),
        ("select-window", "window.select"),
        ("next-window", "window.next"),
        ("previous-window", "window.previous"),
        ("last-window", "window.last"),
        ("rename-window", "window.rename"),
        ("move-window", "window.move"),
        ("tab.create", "window.create"),
        ("tab.select", "window.select"),
        ("tab.next", "window.next"),
        ("tab.previous", "window.previous"),
        ("tab.last", "window.last"),
        ("tab.rename", "window.rename"),
        ("tab.move", "window.move"),
        ("split-window", "pane.split"),
        ("select-pane", "pane.select"),
        ("last-pane", "pane.last"),
        ("resize-pane", "pane.resize"),
        ("kill-pane", "pane.kill"),
        ("pane.read", "terminal.read"),
        ("pane.send_text", "terminal.send_text"),
        ("copy-mode", "terminal.copy_mode"),
        ("events.subscribe", "event.subscribe"),
    ]
}

pub(super) fn explicit_core_commands() -> Vec<(CommandDescriptor, CommandExecutorResolver)> {
    let mut commands = Vec::new();
    let mut add = |id: &str,
                   mutation: MutationClass,
                   arguments: CompactSchema,
                   target: Option<ResourceKind>,
                   executor: CommandExecutorResolver| {
        commands.push((
            core_descriptor(id, id, id, mutation, arguments, target, false),
            executor,
        ));
    };

    for id in [
        "system.ping",
        "system.describe",
        "instance.describe",
        "command.list",
        "command.describe",
        "command.invoke",
        "event.subscribe",
        "event.snapshot",
        "event.rebase",
        "event.unsubscribe",
        "task.status",
        "task.cancel",
    ] {
        add(
            id,
            mutation_for(id),
            direct_control_schema(id),
            None,
            CommandExecutorResolver::DirectControl,
        );
    }
    add(
        "pane.split",
        MutationClass::Write,
        enum_schema("direction", &["right", "down"]),
        Some(ResourceKind::Pane),
        CommandExecutorResolver::SplitPane,
    );
    add(
        "terminal.read",
        MutationClass::Read,
        CompactSchema::default(),
        Some(ResourceKind::Terminal),
        CommandExecutorResolver::ReadTerminal,
    );
    add(
        "terminal.send_text",
        MutationClass::Write,
        CompactSchema {
            arguments: vec![argument("text", ValueType::String)],
        },
        Some(ResourceKind::Terminal),
        CommandExecutorResolver::WriteTerminal,
    );
    add(
        "pane.select",
        MutationClass::Write,
        enum_schema("direction", &["left", "right", "up", "down"]),
        Some(ResourceKind::Pane),
        CommandExecutorResolver::PaneSelect,
    );
    add(
        "pane.last",
        MutationClass::Write,
        CompactSchema::default(),
        Some(ResourceKind::MuxWindow),
        CommandExecutorResolver::PaneLast,
    );
    add(
        "pane.resize",
        MutationClass::Write,
        CompactSchema {
            arguments: vec![argument("adjustment", ValueType::Object)],
        },
        Some(ResourceKind::Pane),
        CommandExecutorResolver::PaneResize,
    );
    add(
        "pane.focus",
        MutationClass::Write,
        CompactSchema::default(),
        Some(ResourceKind::Pane),
        CommandExecutorResolver::Sidebar(crate::app_actions::SidebarAction::FocusTerminal),
    );
    add(
        "session.select",
        MutationClass::Write,
        CompactSchema {
            arguments: vec![argument("selector", ValueType::String)],
        },
        Some(ResourceKind::Binding),
        CommandExecutorResolver::SessionSelect,
    );
    add(
        "session.create",
        MutationClass::Write,
        CompactSchema {
            arguments: vec![argument("launch", ValueType::Object)],
        },
        Some(ResourceKind::Binding),
        CommandExecutorResolver::SessionCreate,
    );
    for (id, executor) in [
        (
            "directory.resolve",
            CommandExecutorResolver::DirectoryResolve,
        ),
        (
            "directory.usage.list",
            CommandExecutorResolver::DirectoryUsageList,
        ),
        ("worktree.list", CommandExecutorResolver::WorktreeList),
        ("worktree.get", CommandExecutorResolver::WorktreeGet),
        ("worktree.create", CommandExecutorResolver::WorktreeCreate),
        ("worktree.remove", CommandExecutorResolver::WorktreeRemove),
    ] {
        add(
            id,
            mutation_for(id),
            resource_command_schema(id),
            None,
            executor,
        );
    }
    commands
}

fn direct_control_schema(id: &str) -> CompactSchema {
    match id {
        "command.describe" => CompactSchema {
            arguments: vec![argument("command", ValueType::String)],
        },
        "command.invoke" => CompactSchema {
            arguments: vec![
                argument("command", ValueType::String),
                repeated_argument("arguments", ValueType::Array),
            ],
        },
        "system.ping" => CompactSchema {
            arguments: vec![
                bounded_optional_integer("minimum_protocol_version", 1),
                bounded_optional_integer("maximum_protocol_version", 1),
            ],
        },
        "event.subscribe" => CompactSchema {
            arguments: vec![
                optional_argument("topics", ValueType::Array),
                optional_argument("scope", ValueType::String),
                optional_argument("subscription", ValueType::ResourceRef),
                bounded_optional_integer("cursor", 0),
            ],
        },
        "event.snapshot" | "event.rebase" | "event.unsubscribe" => CompactSchema {
            arguments: vec![argument("subscription", ValueType::ResourceRef)],
        },
        "task.status" | "task.cancel" => CompactSchema {
            arguments: vec![argument("task", ValueType::ResourceRef)],
        },
        _ => CompactSchema::default(),
    }
}
fn resource_command_schema(id: &str) -> CompactSchema {
    match id {
        "directory.resolve" | "directory.usage.list" | "worktree.list" | "worktree.get" => {
            CompactSchema {
                arguments: vec![argument("path", ValueType::String)],
            }
        }
        "worktree.create" => CompactSchema {
            arguments: vec![
                argument("repository_path", ValueType::String),
                argument("branch", ValueType::String),
                defaulted_argument("managed_by_bootty", ValueType::Boolean, "true"),
            ],
        },
        "worktree.remove" => CompactSchema {
            arguments: vec![
                argument("path", ValueType::String),
                defaulted_argument("force", ValueType::Boolean, "false"),
                optional_argument("confirmation", ValueType::Object),
            ],
        },
        _ => unreachable!("resource executor must declare an argument schema"),
    }
}

fn optional_argument(name: &str, value_type: ValueType) -> ArgumentSchema {
    ArgumentSchema {
        name: name.to_owned(),
        value_type,
        required: false,
        choices: Vec::new(),
        minimum: None,
        maximum: None,
        default: None,
        repeated: false,
    }
}

fn bounded_optional_integer(name: &str, minimum: i64) -> ArgumentSchema {
    let mut argument = optional_argument(name, ValueType::Integer);
    argument.minimum = Some(minimum);
    argument
}

fn defaulted_argument(name: &str, value_type: ValueType, default: &str) -> ArgumentSchema {
    ArgumentSchema {
        name: name.to_owned(),
        value_type,
        required: false,
        choices: Vec::new(),
        minimum: None,
        maximum: None,
        default: Some(default.to_owned()),
        repeated: false,
    }
}

fn repeated_argument(name: &str, value_type: ValueType) -> ArgumentSchema {
    ArgumentSchema {
        name: name.to_owned(),
        value_type,
        required: false,
        choices: Vec::new(),
        minimum: None,
        maximum: None,
        default: None,
        repeated: true,
    }
}

fn enum_schema(name: &str, choices: &[&str]) -> CompactSchema {
    CompactSchema {
        arguments: vec![ArgumentSchema {
            name: name.to_owned(),
            value_type: ValueType::Enum,
            required: true,
            choices: choices.iter().map(|choice| (*choice).to_owned()).collect(),
            minimum: None,
            maximum: None,
            default: None,
            repeated: false,
        }],
    }
}
