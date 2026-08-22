use std::time::Duration;

use serde_json::Value;

use crate::ModuleItem;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SurfacePlacement {
    Status,
    Sidebar,
    Session,
    Floating,
    Docked,
}

impl SurfacePlacement {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "status" => Ok(Self::Status),
            "sidebar" => Ok(Self::Sidebar),
            "session" => Ok(Self::Session),
            "floating" => Ok(Self::Floating),
            "docked" => Ok(Self::Docked),
            _ => Err(format!("invalid extension surface placement {value:?}")),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Sidebar => "sidebar",
            Self::Session => "session",
            Self::Floating => "floating",
            Self::Docked => "docked",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceDeclaration {
    pub id: String,
    pub placement: SurfacePlacement,
    pub order: i32,
    pub interval: Duration,
    /// Window chrome for a floating surface: what to call it, the icon beside the title, and the
    /// key hint along the bottom. A bar or sidebar surface has no chrome of its own and leaves
    /// these unset; a floating surface without a title falls back to its id.
    pub title: Option<String>,
    pub icon: Option<String>,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceSnapshot {
    pub declaration: SurfaceDeclaration,
    pub items: Vec<ModuleItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PublishedSurfaceSnapshot {
    pub module: String,
    pub generation: u64,
    pub snapshot: SurfaceSnapshot,
}

impl PublishedSurfaceSnapshot {
    /// Whether `name` selects this surface. Config names a surface by either identity: the id the
    /// module declared, or the file stem of the module that produced it.
    #[must_use]
    pub fn matches_name(&self, name: &str) -> bool {
        self.snapshot.declaration.id == name
            || std::path::Path::new(&self.module)
                .file_stem()
                .and_then(|stem| stem.to_str())
                == Some(name)
    }

    pub fn items(&self) -> impl Iterator<Item = PublishedSurfaceItem> + '_ {
        self.snapshot
            .items
            .iter()
            .cloned()
            .map(move |item| PublishedSurfaceItem {
                module: self.module.clone(),
                generation: self.generation,
                surface: self.snapshot.declaration.id.clone(),
                item,
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionUiAction {
    pub module: String,
    pub generation: u64,
    pub surface: String,
    pub action: String,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PublishedSurfaceItem {
    pub module: String,
    pub generation: u64,
    pub surface: String,
    pub item: ModuleItem,
}

impl PublishedSurfaceItem {
    #[must_use]
    pub fn action(&self) -> Option<ExtensionUiAction> {
        Some(ExtensionUiAction {
            module: self.module.clone(),
            generation: self.generation,
            surface: self.surface.clone(),
            action: self.item.action.clone()?,
            payload: Value::Null,
        })
    }
}
