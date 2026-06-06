use crate::{
    input_binding::{
        BindingAction, BindingElement, BindingKey, BindingMods, BindingParseError, BindingTrigger,
        InputBinding, parse_binding_elements,
    },
    terminal::KeyInput,
};

#[cfg(test)]
use crate::{
    input_binding::BindingFlags,
    terminal::{KeyMods, TerminalKey},
};

#[derive(Clone, Debug, Default)]
pub struct BindingSet {
    entries: Vec<(BindingTrigger, BindingEntry)>,
    chain_parent: Option<Vec<BindingTrigger>>,
}

#[derive(Clone, Debug)]
enum BindingEntry {
    Leaf(InputBinding),
    Chained {
        binding: InputBinding,
        actions: Vec<BindingAction>,
    },
    Leader(Box<BindingSet>),
}

impl BindingSet {
    pub fn parse_and_put(&mut self, input: &str) -> Result<(), BindingParseError> {
        let elements = parse_binding_elements(input)?;
        if let [BindingElement::Chain(action)] = elements.as_slice() {
            self.append_chain(action.clone())?;
            return Ok(());
        }

        let mut leaders = Vec::new();
        let mut binding = None;
        for element in elements {
            match element {
                BindingElement::Leader(trigger) => leaders.push(trigger),
                BindingElement::Binding(value) => binding = Some(value),
                BindingElement::Chain(_) => return Err(BindingParseError::InvalidFormat),
            }
        }
        let Some(binding) = binding else {
            return Err(BindingParseError::InvalidFormat);
        };
        if leaders.is_empty() {
            self.put(binding);
        } else {
            let mut path = leaders.clone();
            path.push(binding.trigger.clone());
            self.put_sequence(&leaders, binding);
            self.chain_parent = Some(path);
        }
        Ok(())
    }

    pub fn put(&mut self, binding: InputBinding) {
        self.remove(&binding.trigger);
        if binding.action != BindingAction::Unbind {
            self.chain_parent = Some(vec![binding.trigger.clone()]);
            self.entries
                .push((binding.trigger.clone(), BindingEntry::Leaf(binding)));
        }
    }

    pub fn get(&self, trigger: &BindingTrigger) -> Option<&InputBinding> {
        self.entries
            .iter()
            .find_map(|(candidate, entry)| match entry {
                BindingEntry::Leaf(binding) if candidate == trigger => Some(binding),
                BindingEntry::Chained { binding, .. } if candidate == trigger => Some(binding),
                BindingEntry::Leaf(_) | BindingEntry::Chained { .. } | BindingEntry::Leader(_) => {
                    None
                }
            })
    }

    pub fn get_trigger(&self, action: &BindingAction) -> Option<&BindingTrigger> {
        self.entries
            .iter()
            .rev()
            .find_map(|(trigger, entry)| match entry {
                BindingEntry::Leaf(binding)
                    if !binding.flags.performable && binding.action == *action =>
                {
                    Some(trigger)
                }
                BindingEntry::Leaf(_) | BindingEntry::Chained { .. } | BindingEntry::Leader(_) => {
                    None
                }
            })
    }

    pub fn get_event(&self, input: KeyInput) -> Option<&InputBinding> {
        let mods = BindingMods::from(input.mods);
        self.get(&BindingTrigger {
            mods,
            key: BindingKey::Physical(input.key),
        })
        .or_else(|| self.get_codepoint(input, mods))
        .or_else(|| {
            self.get(&BindingTrigger {
                mods,
                key: BindingKey::CatchAll,
            })
        })
        .or_else(|| {
            self.get(&BindingTrigger {
                mods: BindingMods::default(),
                key: BindingKey::CatchAll,
            })
        })
    }

    pub fn remove(&mut self, trigger: &BindingTrigger) {
        let before = self.entries.len();
        self.entries.retain(|(candidate, _)| candidate != trigger);
        if self.entries.len() != before {
            self.chain_parent = None;
        }
    }

    fn get_codepoint(&self, input: KeyInput, mods: BindingMods) -> Option<&InputBinding> {
        let codepoint = input
            .unshifted
            .or_else(|| input.utf8.and_then(single_char))?;
        self.entries.iter().find_map(|(_, entry)| match entry {
            BindingEntry::Leaf(binding)
                if binding.trigger.mods == mods
                    && matches!(
                        binding.trigger.key,
                        BindingKey::Unicode(ch) if char_matches_case_folded(ch, codepoint)
                    ) =>
            {
                Some(binding)
            }
            BindingEntry::Chained { binding, .. }
                if binding.trigger.mods == mods
                    && matches!(
                        binding.trigger.key,
                        BindingKey::Unicode(ch) if char_matches_case_folded(ch, codepoint)
                    ) =>
            {
                Some(binding)
            }
            BindingEntry::Leaf(_) | BindingEntry::Chained { .. } | BindingEntry::Leader(_) => None,
        })
    }

    pub fn chained_actions(&self, trigger: &BindingTrigger) -> Option<&[BindingAction]> {
        self.entries
            .iter()
            .find_map(|(candidate, entry)| match entry {
                BindingEntry::Chained { actions, .. } if candidate == trigger => Some(&**actions),
                BindingEntry::Leaf(_) | BindingEntry::Chained { .. } | BindingEntry::Leader(_) => {
                    None
                }
            })
    }

    pub fn clone_for_config(&self) -> Self {
        Self {
            entries: self
                .entries
                .iter()
                .map(|(trigger, entry)| (trigger.clone(), entry.clone_for_config()))
                .collect(),
            chain_parent: None,
        }
    }

    pub fn format_entries(&self) -> Vec<String> {
        let mut entries = Vec::new();
        self.format_entries_with_prefix(None, &mut entries);
        entries
    }

    fn put_sequence(&mut self, leaders: &[BindingTrigger], binding: InputBinding) {
        let (leader, rest) = leaders.split_first().expect("sequence has a leader");
        if binding.action == BindingAction::Unbind {
            self.remove_sequence(leader, rest, &binding.trigger);
            self.chain_parent = None;
            return;
        }
        let child = self.child_set_mut(leader);
        if rest.is_empty() {
            child.put(binding);
        } else {
            child.put_sequence(rest, binding);
        }
    }

    fn remove_sequence(
        &mut self,
        leader: &BindingTrigger,
        rest: &[BindingTrigger],
        leaf: &BindingTrigger,
    ) {
        let Some(index) = self.entry_index(leader) else {
            return;
        };
        let BindingEntry::Leader(child) = &mut self.entries[index].1 else {
            self.entries.remove(index);
            return;
        };
        if rest.is_empty() {
            child.remove(leaf);
        } else if let Some((next, remaining)) = rest.split_first() {
            child.remove_sequence(next, remaining, leaf);
        }
        if child.entries.is_empty() {
            self.entries.remove(index);
        }
    }

    fn child_set_mut(&mut self, trigger: &BindingTrigger) -> &mut BindingSet {
        let index = match self.entry_index(trigger) {
            Some(index) => index,
            None => {
                self.entries.push((
                    trigger.clone(),
                    BindingEntry::Leader(Box::<BindingSet>::default()),
                ));
                self.entries.len() - 1
            }
        };
        if matches!(self.entries[index].1, BindingEntry::Leaf(_)) {
            self.entries[index].1 = BindingEntry::Leader(Box::<BindingSet>::default());
        }
        if matches!(self.entries[index].1, BindingEntry::Chained { .. }) {
            self.entries[index].1 = BindingEntry::Leader(Box::<BindingSet>::default());
        }
        let BindingEntry::Leader(child) = &mut self.entries[index].1 else {
            unreachable!("entry was normalized to leader");
        };
        child
    }

    fn append_chain(&mut self, action: BindingAction) -> Result<(), BindingParseError> {
        if action == BindingAction::Unbind {
            return Err(BindingParseError::InvalidFormat);
        }
        let path = self
            .chain_parent
            .clone()
            .ok_or(BindingParseError::InvalidFormat)?;
        let Some(entry) = self.entry_mut_at_path(&path) else {
            self.chain_parent = None;
            return Err(BindingParseError::InvalidFormat);
        };
        match entry {
            BindingEntry::Leaf(binding) => {
                let actions = vec![binding.action.clone(), action];
                *entry = BindingEntry::Chained {
                    binding: binding.clone(),
                    actions,
                };
                Ok(())
            }
            BindingEntry::Chained { actions, .. } => {
                actions.push(action);
                Ok(())
            }
            BindingEntry::Leader(_) => Err(BindingParseError::InvalidFormat),
        }
    }

    fn entry_mut_at_path(&mut self, path: &[BindingTrigger]) -> Option<&mut BindingEntry> {
        let (trigger, rest) = path.split_first()?;
        let index = self.entry_index(trigger)?;
        if rest.is_empty() {
            return Some(&mut self.entries[index].1);
        }
        let BindingEntry::Leader(child) = &mut self.entries[index].1 else {
            return None;
        };
        child.entry_mut_at_path(rest)
    }

    fn entry_index(&self, trigger: &BindingTrigger) -> Option<usize> {
        self.entries
            .iter()
            .position(|(candidate, _)| candidate == trigger)
    }

    fn format_entries_with_prefix(&self, prefix: Option<&str>, out: &mut Vec<String>) {
        for (trigger, entry) in &self.entries {
            let trigger_text = match prefix {
                Some(prefix) => format!("{prefix}>{}", trigger.format_entry()),
                None => trigger.format_entry(),
            };
            match entry {
                BindingEntry::Leaf(binding) => {
                    out.push(format!("{trigger_text}={}", binding.action.format_entry()));
                }
                BindingEntry::Chained { actions, .. } => {
                    if let Some((first, rest)) = actions.split_first() {
                        out.push(format!("{trigger_text}={}", first.format_entry()));
                        out.extend(
                            rest.iter()
                                .map(|action| format!("chain={}", action.format_entry())),
                        );
                    }
                }
                BindingEntry::Leader(child) => {
                    child.format_entries_with_prefix(Some(&trigger_text), out);
                }
            }
        }
    }
}

impl BindingEntry {
    fn clone_for_config(&self) -> Self {
        match self {
            Self::Leaf(binding) => Self::Leaf(binding.clone()),
            Self::Chained { binding, actions } => Self::Chained {
                binding: binding.clone(),
                actions: actions.clone(),
            },
            Self::Leader(child) => Self::Leader(Box::new(child.clone_for_config())),
        }
    }
}

fn single_char(input: &str) -> Option<char> {
    let mut chars = input.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(ch)
}

fn char_matches_case_folded(lhs: char, rhs: char) -> bool {
    lhs == rhs || lhs.to_lowercase().eq(rhs.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: TerminalKey, mods: KeyMods) -> KeyInput {
        KeyInput {
            key,
            mods,
            repeat: false,
            utf8: None,
            unshifted: None,
        }
    }

    #[test]
    fn input_binding_set_ports_parse_put_remove_and_flags() {
        let mut set = BindingSet::default();
        set.parse_and_put("unconsumed:a=ignore").unwrap();

        let trigger = BindingTrigger {
            mods: BindingMods::default(),
            key: BindingKey::Unicode('a'),
        };
        assert_eq!(
            set.get(&trigger).unwrap().flags,
            BindingFlags {
                consumed: false,
                ..Default::default()
            }
        );

        set.parse_and_put("a=unbind").unwrap();
        assert_eq!(set.get(&trigger), None);
    }

    #[test]
    fn input_binding_set_ports_event_lookup_precedence() {
        let mut set = BindingSet::default();
        set.parse_and_put("ctrl+quote=ignore").unwrap();
        set.parse_and_put("ctrl+'=reset").unwrap();
        set.parse_and_put("catch_all=text:fallback").unwrap();
        set.parse_and_put("ctrl+catch_all=csi:A").unwrap();

        let ctrl = KeyMods {
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(
            set.get_event(key(TerminalKey::Quote, ctrl)).unwrap().action,
            BindingAction::Ignore
        );
        assert_eq!(
            set.get_event(KeyInput {
                utf8: Some("'"),
                ..key(TerminalKey::A, ctrl)
            })
            .unwrap()
            .action,
            BindingAction::Reset
        );
        assert_eq!(
            set.get_event(KeyInput {
                unshifted: Some('A'),
                ..key(TerminalKey::J, ctrl)
            })
            .unwrap()
            .action,
            BindingAction::Csi("A".to_owned())
        );
        assert_eq!(
            set.get_event(key(TerminalKey::A, KeyMods::default()))
                .unwrap()
                .action,
            BindingAction::Text("fallback".to_owned())
        );
        assert_eq!(
            set.get_event(key(
                TerminalKey::A,
                KeyMods {
                    alt: true,
                    ..Default::default()
                }
            ))
            .unwrap()
            .action,
            BindingAction::Text("fallback".to_owned())
        );
    }

    #[test]
    fn input_binding_set_ports_reverse_lookup_policy() {
        let mut set = BindingSet::default();
        set.parse_and_put("a=reset").unwrap();
        assert_eq!(
            set.get_trigger(&BindingAction::Reset).unwrap().key,
            BindingKey::Unicode('a')
        );

        set.parse_and_put("b=reset").unwrap();
        assert_eq!(
            set.get_trigger(&BindingAction::Reset).unwrap().key,
            BindingKey::Unicode('b')
        );

        set.parse_and_put("b=unbind").unwrap();
        assert_eq!(
            set.get_trigger(&BindingAction::Reset).unwrap().key,
            BindingKey::Unicode('a')
        );

        set.parse_and_put("performable:c=reset").unwrap();
        assert_eq!(
            set.get_trigger(&BindingAction::Reset).unwrap().key,
            BindingKey::Unicode('a')
        );

        set.parse_and_put("a=text:old").unwrap();
        assert_eq!(set.get_trigger(&BindingAction::Reset), None);
    }

    #[test]
    fn input_binding_set_ports_chained_reverse_lookup_restoration() {
        let mut set = BindingSet::default();
        set.parse_and_put("a=reset").unwrap();
        set.parse_and_put("b=reset").unwrap();
        assert_eq!(
            set.get_trigger(&BindingAction::Reset).unwrap().key,
            BindingKey::Unicode('b')
        );

        set.parse_and_put("chain=text:next").unwrap();
        assert_eq!(
            set.get_trigger(&BindingAction::Reset).unwrap().key,
            BindingKey::Unicode('a')
        );

        set.parse_and_put("performable:c=clear_screen").unwrap();
        set.parse_and_put("chain=text:performable").unwrap();
        assert_eq!(
            set.get_trigger(&BindingAction::Reset).unwrap().key,
            BindingKey::Unicode('a')
        );
    }

    #[test]
    fn input_binding_set_ports_sequence_tree_storage() {
        let mut set = BindingSet::default();
        set.parse_and_put("a>b=reset").unwrap();
        set.parse_and_put("a>c=text:next").unwrap();

        let BindingEntry::Leader(child) = &set.entries[0].1 else {
            panic!("expected sequence leader");
        };
        assert_eq!(
            child
                .get(&BindingTrigger {
                    mods: BindingMods::default(),
                    key: BindingKey::Unicode('b')
                })
                .unwrap()
                .action,
            BindingAction::Reset
        );
        assert_eq!(
            child
                .get(&BindingTrigger {
                    mods: BindingMods::default(),
                    key: BindingKey::Unicode('c')
                })
                .unwrap()
                .action,
            BindingAction::Text("next".to_owned())
        );

        set.parse_and_put("a>b=text:updated").unwrap();
        let BindingEntry::Leader(child) = &set.entries[0].1 else {
            panic!("expected sequence leader");
        };
        assert_eq!(
            child
                .get(&BindingTrigger {
                    mods: BindingMods::default(),
                    key: BindingKey::Unicode('b')
                })
                .unwrap()
                .action,
            BindingAction::Text("updated".to_owned())
        );
    }

    #[test]
    fn input_binding_set_ports_sequence_unbind_cleanup() {
        let mut set = BindingSet::default();
        set.parse_and_put("a=reset").unwrap();
        set.parse_and_put("a>b=text:leaf").unwrap();
        assert_eq!(set.get_trigger(&BindingAction::Reset), None);
        assert_eq!(
            set.get(&BindingTrigger {
                mods: BindingMods::default(),
                key: BindingKey::Unicode('a')
            }),
            None
        );

        set.parse_and_put("a>b=unbind").unwrap();
        assert!(set.entries.is_empty());

        set.parse_and_put("a>b=unbind").unwrap();
        assert!(set.entries.is_empty());
    }

    #[test]
    fn input_binding_set_ports_chained_actions() {
        let mut set = BindingSet::default();
        set.parse_and_put("unconsumed:a=reset").unwrap();
        set.parse_and_put("chain=text:next").unwrap();
        set.parse_and_put("chain=csi:0m").unwrap();

        let trigger = BindingTrigger {
            mods: BindingMods::default(),
            key: BindingKey::Unicode('a'),
        };
        assert_eq!(
            set.chained_actions(&trigger).unwrap(),
            &[
                BindingAction::Reset,
                BindingAction::Text("next".to_owned()),
                BindingAction::Csi("0m".to_owned())
            ]
        );
        assert!(!set.get(&trigger).unwrap().flags.consumed);
        assert_eq!(set.get_trigger(&BindingAction::Reset), None);

        let mut sequence = BindingSet::default();
        sequence.parse_and_put("a>b=reset").unwrap();
        sequence.parse_and_put("chain=text:after").unwrap();
        let BindingEntry::Leader(child) = &sequence.entries[0].1 else {
            panic!("expected sequence leader");
        };
        assert_eq!(
            child
                .chained_actions(&BindingTrigger {
                    mods: BindingMods::default(),
                    key: BindingKey::Unicode('b')
                })
                .unwrap(),
            &[
                BindingAction::Reset,
                BindingAction::Text("after".to_owned())
            ]
        );
    }

    #[test]
    fn input_binding_set_rejects_invalid_chains() {
        let mut set = BindingSet::default();
        assert_eq!(
            set.parse_and_put("chain=text:orphan"),
            Err(BindingParseError::InvalidFormat)
        );

        set.parse_and_put("a=reset").unwrap();
        set.parse_and_put("a=unbind").unwrap();
        assert_eq!(
            set.parse_and_put("chain=text:after_unbind"),
            Err(BindingParseError::InvalidFormat)
        );

        set.parse_and_put("a=reset").unwrap();
        assert_eq!(
            set.parse_and_put("chain=unbind"),
            Err(BindingParseError::InvalidFormat)
        );
    }

    #[test]
    fn input_binding_set_ports_clone_for_config() {
        let mut set = BindingSet::default();
        set.parse_and_put("a=text:hello").unwrap();
        set.parse_and_put("chain=text:world").unwrap();
        set.parse_and_put("b>c=reset").unwrap();
        assert!(set.chain_parent.is_some());

        let cloned = set.clone_for_config();
        assert!(cloned.chain_parent.is_none());
        assert!(
            cloned
                .entries
                .iter()
                .filter_map(|(_, entry)| match entry {
                    BindingEntry::Leader(child) => Some(child),
                    BindingEntry::Leaf(_) | BindingEntry::Chained { .. } => None,
                })
                .all(|child| child.chain_parent.is_none())
        );
        assert_eq!(
            cloned
                .chained_actions(&BindingTrigger {
                    mods: BindingMods::default(),
                    key: BindingKey::Unicode('a')
                })
                .unwrap(),
            &[
                BindingAction::Text("hello".to_owned()),
                BindingAction::Text("world".to_owned())
            ]
        );
    }

    #[test]
    fn input_binding_set_ports_format_entries() {
        let mut set = BindingSet::default();
        set.parse_and_put("a=text:hello").unwrap();
        set.parse_and_put("chain=text:world").unwrap();
        set.parse_and_put("ctrl+b=reset").unwrap();
        set.parse_and_put("c>d=csi:0m").unwrap();

        assert_eq!(
            set.format_entries(),
            vec![
                "a=text:hello".to_owned(),
                "chain=text:world".to_owned(),
                "ctrl+b=reset".to_owned(),
                "c>d=csi:0m".to_owned()
            ]
        );
    }
}
