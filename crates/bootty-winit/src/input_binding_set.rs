use crate::{
    input_binding::{
        BindingAction, BindingElement, BindingKey, BindingMods, BindingParseError, BindingTrigger,
        InputBinding, parse_binding_elements,
    },
    terminal::KeyInput,
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
            let trigger = binding.trigger.clone();
            self.put_sequence(&leaders, binding);
            leaders.push(trigger);
            self.chain_parent = Some(leaders);
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
        self.entries.iter().find_map(|(candidate, entry)| {
            (candidate == trigger)
                .then_some(entry)
                .and_then(BindingEntry::binding)
        })
    }

    pub fn get_trigger(&self, action: &BindingAction) -> Option<&BindingTrigger> {
        self.entries.iter().rev().find_map(|(trigger, entry)| {
            let BindingEntry::Leaf(binding) = entry else {
                return None;
            };
            (!binding.flags.performable && binding.action == *action).then_some(trigger)
        })
    }

    pub fn get_event(&self, input: KeyInput) -> Option<&InputBinding> {
        let mod_candidates = BindingTrigger::input_mod_candidates(input);
        self.get_with_mod_candidates(&mod_candidates, BindingKey::Physical(input.key))
            .or_else(|| self.get_codepoint(input, &mod_candidates))
            .or_else(|| self.get_with_mod_candidates(&mod_candidates, BindingKey::CatchAll))
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

    fn get_with_mod_candidates(
        &self,
        mod_candidates: &[BindingMods],
        key: BindingKey,
    ) -> Option<&InputBinding> {
        mod_candidates.iter().find_map(|mods| {
            self.get(&BindingTrigger {
                mods: *mods,
                key: key.clone(),
            })
        })
    }

    fn get_codepoint(
        &self,
        input: KeyInput,
        mod_candidates: &[BindingMods],
    ) -> Option<&InputBinding> {
        let codepoint = input
            .unshifted
            .or_else(|| input.utf8.and_then(single_char))?;
        mod_candidates.iter().find_map(|mods| {
            self.entries.iter().find_map(|(_, entry)| {
                let binding = entry.binding()?;
                (binding.trigger.mods == *mods
                    && matches!(
                        binding.trigger.key,
                        BindingKey::Unicode(ch) if char_matches_case_folded(ch, codepoint)
                    ))
                .then_some(binding)
            })
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
        } else {
            let (next, remaining) = rest.split_first().expect("rest is not empty");
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
        if !matches!(self.entries[index].1, BindingEntry::Leader(_)) {
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
            }
            BindingEntry::Chained { actions, .. } => {
                actions.push(action);
            }
            BindingEntry::Leader(_) => return Err(BindingParseError::InvalidFormat),
        }
        Ok(())
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
                    let Some((first, rest)) = actions.split_first() else {
                        continue;
                    };
                    out.push(format!("{trigger_text}={}", first.format_entry()));
                    out.extend(
                        rest.iter()
                            .map(|action| format!("chain={}", action.format_entry())),
                    );
                }
                BindingEntry::Leader(child) => {
                    child.format_entries_with_prefix(Some(&trigger_text), out);
                }
            }
        }
    }
}

impl BindingEntry {
    fn binding(&self) -> Option<&InputBinding> {
        match self {
            Self::Leaf(binding) | Self::Chained { binding, .. } => Some(binding),
            Self::Leader(_) => None,
        }
    }

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
