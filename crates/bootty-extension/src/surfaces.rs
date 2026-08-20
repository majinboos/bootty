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
    #[must_use]
    pub fn into_items(self) -> Vec<PublishedSurfaceItem> {
        let surface = self.snapshot.declaration.id;
        self.snapshot
            .items
            .into_iter()
            .map(|item| PublishedSurfaceItem {
                module: self.module.clone(),
                generation: self.generation,
                surface: surface.clone(),
                item,
            })
            .collect()
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
