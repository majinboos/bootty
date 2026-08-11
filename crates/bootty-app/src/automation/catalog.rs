//! Checked-in, backend-neutral command catalog and source-parity manifests.
//!
//! The manifests are deliberately data rather than generated Rust lists: they are
//! inspectable in review, pin the upstream source inventories, and make an
//! upstream addition fail at the catalog seam instead of becoming an unknown
//! command at runtime.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::LazyLock,
};

use serde::{Deserialize, Serialize};

const EXPECTED_DESCRIPTOR_COUNT: usize = 310;
const EXPECTED_NAMESPACE_COUNT: usize = 39;
const HERDR_REVISION: &str = "2863b715132fe29e53089e06f105943d1df0b3b4";
const RMUX_PINNED_VERSION: &str = "0.9.1";
const RMUX_RUNTIME_ADAPTER: &str = "embedded rmux 0.10 SDK/IPC only; never the standalone rmux CLI";

const CATALOG_MANIFEST: &str = include_str!("../../catalog-manifests/canonical-catalog.json");
const SOURCE_MANIFESTS: [(&str, &str); 7] = [
    (
        "bootty_actions",
        include_str!("../../catalog-manifests/bootty-actions.json"),
    ),
    (
        "bundled_extensions",
        include_str!("../../catalog-manifests/bundled-agent-extension.json"),
    ),
    (
        "control_plane",
        include_str!("../../catalog-manifests/control-plane.json"),
    ),
    (
        "herdr_methods",
        include_str!("../../catalog-manifests/herdr-methods-2863b715.json"),
    ),
    (
        "rmux_cli_signatures_0_9_1",
        include_str!("../../catalog-manifests/rmux-cli-signatures-0.9.1.json"),
    ),
    (
        "rmux_requests_0_9_1",
        include_str!("../../catalog-manifests/rmux-requests-0.9.1.json"),
    ),
    (
        "rmux_sdk_0_9_1",
        include_str!("../../catalog-manifests/rmux-sdk-operations-0.9.1.json"),
    ),
];

const SERVICE_REQUIRED_MANIFEST: &str =
    include_str!("../../catalog-manifests/service-required.json");

/// A parsed, validated catalog built from the checked-in manifests.
#[derive(Clone, Debug)]
pub struct CanonicalCatalog {
    document: CatalogDocument,
    descriptor_indices: BTreeMap<String, usize>,
    aliases: BTreeMap<String, String>,
    source_manifests: BTreeMap<String, SourceManifest>,
    service_required: BTreeMap<String, ServiceRequiredRecord>,
}

/// Public shorthand for the canonical catalog.
pub type Catalog = CanonicalCatalog;

#[derive(Clone, Debug, Deserialize)]
struct CatalogDocument {
    schema_version: u32,
    expected_descriptor_count: usize,
    expected_namespace_count: usize,
    expected_source_entry_count: usize,
    source_manifest_inventory: BTreeMap<String, SourceManifestInventory>,
    namespaces: Vec<String>,
    descriptors: Vec<CanonicalDescriptor>,
}

#[derive(Clone, Debug, Deserialize)]
struct SourceManifestInventory {
    entry_ids: Vec<String>,
    #[serde(default)]
    service_required: BTreeMap<String, String>,
}

/// One of the 310 canonical command descriptors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalDescriptor {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub origin: CatalogOrigin,
    #[serde(default)]
    pub argument_schema: Vec<CatalogArgumentSchema>,
    pub result_schema: CatalogResultSchema,
    #[serde(default)]
    pub targets: Vec<CatalogTarget>,
    pub availability: BackendAvailability,
    pub mutation: CatalogMutation,
    pub palette: CatalogPaletteMetadata,
}

/// Provenance for a descriptor. Extension-origin entries are data; core does not
/// acquire a Rust dependency on an extension's domain model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogOrigin {
    Core,
    Extension {
        extension_id: String,
        generation: u64,
    },
}

/// Small schema vocabulary used by the catalog, dynamic CLI, and help surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogValueType {
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Enum,
    Array,
    Object,
    ResourceRef,
    Json,
}

/// A positional command argument. `repeated` is only valid on the final field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogArgumentSchema {
    pub name: String,
    #[serde(rename = "type")]
    pub value_type: CatalogValueType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default)]
    pub repeated: bool,
}

/// Result documentation. The registry intentionally supports only the compact
/// vocabulary needed for parsing and help; this is not a JSON Schema engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogResultSchema {
    #[serde(rename = "type")]
    pub value_type: CatalogValueType,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, CatalogResultSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<CatalogResultSchema>>,
}

/// A resource family a catalog command can target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogTarget {
    Instance,
    ApplicationWindow,
    Binding,
    Space,
    Session,
    Window,
    Pane,
    Terminal,
    Client,
    Directory,
    Worktree,
    Task,
    Subscription,
    Surface,
    Extension,
}

/// The static availability declaration for the core dispatcher and backends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendAvailability {
    pub core: CatalogAvailability,
    pub native: CatalogAvailability,
    pub rmux: CatalogAvailability,
    pub tmux: CatalogAvailability,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogAvailability {
    Available,
    Conditional,
    Unsupported,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogMutation {
    Read,
    Write,
    Destructive,
}

/// Metadata that keeps palette filtering in the same registry without making
/// every externally discoverable command a palette entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPaletteMetadata {
    pub visible: bool,
    pub category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

/// A checked-in upstream source manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceManifest {
    pub schema_version: u32,
    pub manifest: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub source_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_adapter: Option<String>,
    pub entries: Vec<SourceManifestEntry>,
}

/// Public shorthand for a source manifest.
pub type CatalogSource = SourceManifest;

/// One source operation's explicit relation to the canonical catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceManifestEntry {
    pub id: String,
    pub kind: SourceMappingKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
}

/// Public shorthand for a source mapping.
pub type CatalogSourceMapping = SourceManifestEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceMappingKind {
    Descriptor,
    Alias,
    Unsupported,
    ServiceRequired,
}

#[derive(Clone, Debug, Deserialize)]
struct ServiceRequiredDocument {
    schema_version: u32,
    records: Vec<ServiceRequiredRecord>,
}

/// One deliberate service dependency shared by one or more pinned source spellings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRequiredRecord {
    pub id: String,
    pub reason: String,
}

/// Deterministic catalog-completeness report for tests and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogCompleteness {
    pub descriptor_count: usize,
    pub namespace_count: usize,
    pub source_entry_count: usize,
    pub service_required_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogError {
    message: String,
}

impl CatalogError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CatalogError {}

impl CanonicalCatalog {
    /// Parses and validates every checked-in manifest without consulting a live
    /// backend or an upstream checkout.
    pub fn load_checked_in() -> Result<Self, CatalogError> {
        let document = serde_json::from_str::<CatalogDocument>(CATALOG_MANIFEST)
            .map_err(|error| CatalogError::new(format!("invalid canonical catalog: {error}")))?;
        let service_required =
            serde_json::from_str::<ServiceRequiredDocument>(SERVICE_REQUIRED_MANIFEST).map_err(
                |error| CatalogError::new(format!("invalid service-required manifest: {error}")),
            )?;
        let mut source_manifests = BTreeMap::new();
        for (name, source) in SOURCE_MANIFESTS {
            let manifest = serde_json::from_str::<SourceManifest>(source).map_err(|error| {
                CatalogError::new(format!("invalid source manifest {name}: {error}"))
            })?;
            if manifest.manifest != name {
                return Err(CatalogError::new(format!(
                    "source manifest {name} declares {}",
                    manifest.manifest
                )));
            }
            if source_manifests.insert(name.to_owned(), manifest).is_some() {
                return Err(CatalogError::new(format!(
                    "duplicate source manifest {name}"
                )));
            }
        }
        Self::from_documents(document, source_manifests, service_required)
    }

    fn from_documents(
        document: CatalogDocument,
        source_manifests: BTreeMap<String, SourceManifest>,
        service_required_document: ServiceRequiredDocument,
    ) -> Result<Self, CatalogError> {
        if document.schema_version != 1 {
            return Err(CatalogError::new(format!(
                "unsupported canonical catalog schema version {}",
                document.schema_version
            )));
        }
        if document.expected_descriptor_count != EXPECTED_DESCRIPTOR_COUNT
            || document.descriptors.len() != EXPECTED_DESCRIPTOR_COUNT
        {
            return Err(CatalogError::new(format!(
                "catalog has {} descriptors; expected {EXPECTED_DESCRIPTOR_COUNT}",
                document.descriptors.len()
            )));
        }
        if document.expected_namespace_count != EXPECTED_NAMESPACE_COUNT {
            return Err(CatalogError::new(format!(
                "catalog declares {} namespaces; expected {EXPECTED_NAMESPACE_COUNT}",
                document.expected_namespace_count
            )));
        }

        let declared_namespaces = document.namespaces.iter().cloned().collect::<BTreeSet<_>>();
        let actual_namespaces = document
            .descriptors
            .iter()
            .map(|descriptor| namespace(&descriptor.id))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if declared_namespaces.len() != EXPECTED_NAMESPACE_COUNT
            || actual_namespaces.len() != EXPECTED_NAMESPACE_COUNT
            || declared_namespaces != actual_namespaces
        {
            return Err(CatalogError::new(format!(
                "catalog namespace mismatch: declared {}, actual {}; expected {EXPECTED_NAMESPACE_COUNT}",
                declared_namespaces.len(),
                actual_namespaces.len()
            )));
        }

        let mut descriptor_indices = BTreeMap::new();
        for (index, descriptor) in document.descriptors.iter().enumerate() {
            validate_descriptor(descriptor)?;
            if descriptor_indices
                .insert(descriptor.id.clone(), index)
                .is_some()
            {
                return Err(CatalogError::new(format!(
                    "duplicate canonical descriptor {}",
                    descriptor.id
                )));
            }
        }

        let mut aliases = BTreeMap::new();
        for descriptor in &document.descriptors {
            for alias in &descriptor.aliases {
                if descriptor_indices.contains_key(alias) {
                    return Err(CatalogError::new(format!(
                        "alias {alias} collides with a canonical descriptor"
                    )));
                }
                if aliases
                    .insert(alias.clone(), descriptor.id.clone())
                    .is_some()
                {
                    return Err(CatalogError::new(format!("duplicate alias {alias}")));
                }
            }
        }

        let service_required = validate_service_required_records(service_required_document)?;
        validate_source_manifests(
            &source_manifests,
            &document.source_manifest_inventory,
            document.expected_source_entry_count,
            &descriptor_indices,
            &aliases,
            &service_required,
        )?;

        Ok(Self {
            document,
            descriptor_indices,
            aliases,
            source_manifests,
            service_required,
        })
    }

    /// Returns descriptors in the normative proposal's stable order.
    pub fn descriptors(&self) -> impl Iterator<Item = &CanonicalDescriptor> {
        self.document.descriptors.iter()
    }

    /// Resolves a canonical name or stable source alias.
    pub fn descriptor(&self, name: &str) -> Option<&CanonicalDescriptor> {
        let canonical = self.canonical_id(name)?;
        self.descriptor_indices
            .get(canonical)
            .and_then(|index| self.document.descriptors.get(*index))
    }

    /// Returns the canonical command name for a canonical name or alias.
    pub fn canonical_id(&self, name: &str) -> Option<&str> {
        self.descriptor_indices
            .get_key_value(name)
            .map(|(canonical, _)| canonical.as_str())
            .or_else(|| self.aliases.get(name).map(String::as_str))
    }

    /// Looks up one checked-in source manifest by its stable manifest name.
    pub fn source_manifest(&self, name: &str) -> Option<&SourceManifest> {
        self.source_manifests.get(name)
    }

    /// Looks up the explicit disposition of one pinned source operation.
    pub fn source_mapping(
        &self,
        manifest: &str,
        source_operation: &str,
    ) -> Option<&SourceManifestEntry> {
        self.source_manifest(manifest)?
            .entries
            .iter()
            .find(|entry| entry.id == source_operation)
    }

    /// Looks up a deliberate service dependency referenced by source mappings.
    pub fn service_required(&self, id: &str) -> Option<&ServiceRequiredRecord> {
        self.service_required.get(id)
    }

    /// Iterates source manifests in deterministic manifest-name order.
    pub fn source_manifests(&self) -> impl Iterator<Item = &SourceManifest> {
        self.source_manifests.values()
    }

    pub fn completeness(&self) -> CatalogCompleteness {
        let source_entry_count = self
            .source_manifests
            .values()
            .map(|manifest| manifest.entries.len())
            .sum();
        let service_required_count = self.service_required.len();
        CatalogCompleteness {
            descriptor_count: self.document.descriptors.len(),
            namespace_count: self.document.namespaces.len(),
            source_entry_count,
            service_required_count,
        }
    }
}

/// Loads the process-wide immutable catalog. Parsing failures are programmer
/// errors because every manifest is checked into this crate and validated here.
pub fn canonical_catalog() -> &'static CanonicalCatalog {
    static CATALOG: LazyLock<CanonicalCatalog> = LazyLock::new(|| {
        CanonicalCatalog::load_checked_in()
            .unwrap_or_else(|error| panic!("invalid checked-in command catalog: {error}"))
    });
    &CATALOG
}

fn namespace(id: &str) -> Result<String, CatalogError> {
    let (namespace, remainder) = id
        .split_once('.')
        .ok_or_else(|| CatalogError::new(format!("descriptor {id} has no namespace")))?;
    if namespace.is_empty() || remainder.is_empty() {
        return Err(CatalogError::new(format!(
            "descriptor {id} has an empty namespace or command segment"
        )));
    }
    Ok(namespace.to_owned())
}

fn validate_descriptor(descriptor: &CanonicalDescriptor) -> Result<(), CatalogError> {
    let _ = namespace(&descriptor.id)?;
    if descriptor.title.is_empty() || descriptor.description.is_empty() {
        return Err(CatalogError::new(format!(
            "descriptor {} has an empty title or description",
            descriptor.id
        )));
    }
    let mut seen_arguments = BTreeSet::new();
    for (index, argument) in descriptor.argument_schema.iter().enumerate() {
        if argument.name.is_empty() || !seen_arguments.insert(&argument.name) {
            return Err(CatalogError::new(format!(
                "descriptor {} has an invalid argument name",
                descriptor.id
            )));
        }
        if argument.repeated && index + 1 != descriptor.argument_schema.len() {
            return Err(CatalogError::new(format!(
                "descriptor {} has a non-trailing repeated argument {}",
                descriptor.id, argument.name
            )));
        }
        if argument.value_type == CatalogValueType::Enum && argument.choices.is_empty() {
            return Err(CatalogError::new(format!(
                "descriptor {} enum argument {} has no choices",
                descriptor.id, argument.name
            )));
        }
        if argument
            .minimum
            .is_some_and(|minimum| argument.maximum.is_some_and(|maximum| minimum > maximum))
        {
            return Err(CatalogError::new(format!(
                "descriptor {} has an inverted range for {}",
                descriptor.id, argument.name
            )));
        }
    }
    if descriptor.id.starts_with("agents.")
        && (!matches!(
            &descriptor.origin,
            CatalogOrigin::Extension {
                extension_id,
                generation: _
            } if extension_id == "bootty.agents"
        ) || descriptor.availability.core != CatalogAvailability::Unavailable)
    {
        return Err(CatalogError::new(format!(
            "agent descriptor {} must be an unavailable bootty.agents extension placeholder",
            descriptor.id
        )));
    }
    Ok(())
}

fn validate_service_required_records(
    document: ServiceRequiredDocument,
) -> Result<BTreeMap<String, ServiceRequiredRecord>, CatalogError> {
    if document.schema_version != 1 || document.records.len() != 1 {
        return Err(CatalogError::new(
            "service-required manifest must contain exactly one schema-v1 record",
        ));
    }
    let mut records = BTreeMap::new();
    for record in document.records {
        if record.id.is_empty()
            || record.reason.is_empty()
            || records.insert(record.id.clone(), record).is_some()
        {
            return Err(CatalogError::new(
                "service-required manifest has an incomplete or duplicate record",
            ));
        }
    }
    if !records.contains_key("rmux.web-share") {
        return Err(CatalogError::new(
            "rmux.web-share must be the sole deliberate service requirement",
        ));
    }
    Ok(records)
}

fn validate_source_manifests(
    source_manifests: &BTreeMap<String, SourceManifest>,
    source_manifest_inventory: &BTreeMap<String, SourceManifestInventory>,
    expected_source_entry_count: usize,
    descriptor_indices: &BTreeMap<String, usize>,
    aliases: &BTreeMap<String, String>,
    service_required: &BTreeMap<String, ServiceRequiredRecord>,
) -> Result<(), CatalogError> {
    let inventory_entry_count: usize = source_manifest_inventory
        .values()
        .map(|inventory| inventory.entry_ids.len())
        .sum();
    if inventory_entry_count != expected_source_entry_count {
        return Err(CatalogError::new(format!(
            "pinned source inventory has {inventory_entry_count} entries; expected {expected_source_entry_count}"
        )));
    }

    let actual = source_manifests
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected = source_manifest_inventory
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(CatalogError::new(
            "source manifest set does not match the pinned inventory",
        ));
    }

    let mut source_entry_count = 0;
    let mut service_mappings = BTreeSet::new();
    let mut expected_service_mappings = BTreeSet::new();
    for (name, manifest) in source_manifests {
        let inventory = source_manifest_inventory.get(name).ok_or_else(|| {
            CatalogError::new(format!("pinned source inventory has no manifest {name}"))
        })?;
        let expected_entry_ids = inventory
            .entry_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if expected_entry_ids.len() != inventory.entry_ids.len() || expected_entry_ids.contains("")
        {
            return Err(CatalogError::new(format!(
                "pinned source inventory {name} has a missing or duplicate entry id"
            )));
        }
        for (id, service) in &inventory.service_required {
            if id.is_empty()
                || service.is_empty()
                || !expected_entry_ids.contains(id.as_str())
                || !service_required.contains_key(service)
            {
                return Err(CatalogError::new(format!(
                    "pinned source inventory {name} has an invalid service mapping"
                )));
            }
            expected_service_mappings.insert((name.as_str(), id.as_str(), service.as_str()));
        }

        if manifest.schema_version != 1 || manifest.entries.is_empty() {
            return Err(CatalogError::new(format!(
                "source manifest {name} has an invalid schema version or no entries"
            )));
        }
        validate_source_provenance(name, manifest)?;
        let entry_ids = manifest
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<BTreeSet<_>>();
        if entry_ids.len() != manifest.entries.len() || entry_ids.contains("") {
            return Err(CatalogError::new(format!(
                "source manifest {name} has a missing or duplicate entry id"
            )));
        }
        if entry_ids != expected_entry_ids {
            return Err(CatalogError::new(format!(
                "source manifest {name} entry IDs do not match the pinned inventory"
            )));
        }
        source_entry_count += manifest.entries.len();

        for entry in &manifest.entries {
            match entry.kind {
                SourceMappingKind::Descriptor => {
                    let command = required_mapping_command(name, entry)?;
                    if !descriptor_indices.contains_key(command) {
                        return Err(CatalogError::new(format!(
                            "source manifest {name} maps {} to missing descriptor {command}",
                            entry.id
                        )));
                    }
                }
                SourceMappingKind::Alias => {
                    let command = required_mapping_command(name, entry)?;
                    if aliases.get(&entry.id).map(String::as_str) != Some(command) {
                        return Err(CatalogError::new(format!(
                            "source manifest {name} alias {} is not registered for {command}",
                            entry.id
                        )));
                    }
                }
                SourceMappingKind::Unsupported => validate_unsupported(name, entry)?,
                SourceMappingKind::ServiceRequired => {
                    let service = validate_service_mapping(name, entry, service_required)?;
                    service_mappings.insert((name.as_str(), entry.id.as_str(), service));
                }
            }
        }
    }
    if source_entry_count != expected_source_entry_count {
        return Err(CatalogError::new(format!(
            "source manifests have {source_entry_count} entries; expected {expected_source_entry_count}"
        )));
    }
    if service_mappings != expected_service_mappings {
        return Err(CatalogError::new(
            "source service mappings do not match the pinned inventory",
        ));
    }
    Ok(())
}

fn validate_source_provenance(
    manifest_name: &str,
    manifest: &SourceManifest,
) -> Result<(), CatalogError> {
    if manifest_name == "herdr_methods" && manifest.revision.as_deref() != Some(HERDR_REVISION) {
        return Err(CatalogError::new(format!(
            "source manifest {manifest_name} does not pin HerdR revision {HERDR_REVISION}"
        )));
    }
    if matches!(
        manifest_name,
        "rmux_cli_signatures_0_9_1" | "rmux_requests_0_9_1" | "rmux_sdk_0_9_1"
    ) && (manifest.version.as_deref() != Some(RMUX_PINNED_VERSION)
        || manifest.runtime_adapter.as_deref() != Some(RMUX_RUNTIME_ADAPTER))
    {
        return Err(CatalogError::new(format!(
            "source manifest {manifest_name} must describe rmux {RMUX_PINNED_VERSION} with the embedded SDK/IPC adapter"
        )));
    }
    Ok(())
}

fn required_mapping_command<'a>(
    manifest: &str,
    entry: &'a SourceManifestEntry,
) -> Result<&'a str, CatalogError> {
    if entry.reason.is_some() || entry.service.is_some() {
        return Err(CatalogError::new(format!(
            "source manifest {manifest} entry {} has fields for a different mapping kind",
            entry.id
        )));
    }
    entry
        .command
        .as_deref()
        .filter(|command| !command.is_empty())
        .ok_or_else(|| {
            CatalogError::new(format!(
                "source manifest {manifest} entry {} has no canonical command",
                entry.id
            ))
        })
}

fn validate_unsupported(manifest: &str, entry: &SourceManifestEntry) -> Result<(), CatalogError> {
    if entry.command.is_some()
        || entry.service.is_some()
        || entry.reason.as_deref().is_none_or(str::is_empty)
    {
        return Err(CatalogError::new(format!(
            "source manifest {manifest} unsupported entry {} is incomplete",
            entry.id
        )));
    }
    Ok(())
}

fn validate_service_mapping<'a>(
    manifest: &str,
    entry: &'a SourceManifestEntry,
    service_required: &BTreeMap<String, ServiceRequiredRecord>,
) -> Result<&'a str, CatalogError> {
    let service = entry
        .service
        .as_deref()
        .filter(|service| !service.is_empty())
        .ok_or_else(|| {
            CatalogError::new(format!(
                "source manifest {manifest} service-required entry {} has no service record",
                entry.id
            ))
        })?;
    if entry.command.is_some()
        || entry.reason.is_some()
        || !service_required.contains_key(service)
        || service != "rmux.web-share"
    {
        return Err(CatalogError::new(format!(
            "only rmux.web-share may be service_required; found {manifest}:{}",
            entry.id
        )));
    }
    Ok(service)
}

#[cfg(test)]
mod tests {
    use crate::{app_actions::SidebarAction, commands::CommandRegistry};

    use super::*;

    #[test]
    fn checked_in_catalog_has_the_normative_cardinality() {
        let catalog = CanonicalCatalog::load_checked_in().expect("checked-in catalog");

        let completeness = catalog.completeness();
        assert_eq!(completeness.descriptor_count, 310);
        assert_eq!(completeness.namespace_count, 39);
        assert_eq!(
            completeness.source_entry_count, catalog.document.expected_source_entry_count,
            "source entry count must match the pinned source inventory"
        );
        assert_eq!(completeness.service_required_count, 1);
    }

    #[test]
    fn source_inventory_rejects_a_missing_non_final_ordinary_entry() {
        let document = serde_json::from_str::<CatalogDocument>(CATALOG_MANIFEST)
            .expect("canonical catalog document");
        let service_required_document =
            serde_json::from_str::<ServiceRequiredDocument>(SERVICE_REQUIRED_MANIFEST)
                .expect("service-required document");
        let mut source_manifests = CanonicalCatalog::load_checked_in()
            .expect("checked-in catalog")
            .source_manifests;
        let entries = &mut source_manifests
            .get_mut("rmux_sdk_0_9_1")
            .expect("rmux SDK source manifest")
            .entries;
        let index = entries
            .iter()
            .position(|entry| entry.id == "Pane::capture_region")
            .expect("ordinary source entry");
        assert!(
            index + 1 < entries.len(),
            "regression must remove a non-final source entry"
        );
        let removed = entries.remove(index);
        assert_eq!(removed.kind, SourceMappingKind::Descriptor);

        let error =
            CanonicalCatalog::from_documents(document, source_manifests, service_required_document)
                .expect_err("missing ordinary source entry must fail completeness");

        assert!(
            error
                .to_string()
                .contains("entry IDs do not match the pinned inventory")
        );
    }

    #[test]
    fn source_inventory_rejects_a_reclassified_service_mapping() {
        let document = serde_json::from_str::<CatalogDocument>(CATALOG_MANIFEST)
            .expect("canonical catalog document");
        let service_required_document =
            serde_json::from_str::<ServiceRequiredDocument>(SERVICE_REQUIRED_MANIFEST)
                .expect("service-required document");
        let mut source_manifests = CanonicalCatalog::load_checked_in()
            .expect("checked-in catalog")
            .source_manifests;
        let entry = source_manifests
            .get_mut("rmux_sdk_0_9_1")
            .expect("rmux SDK source manifest")
            .entries
            .iter_mut()
            .find(|entry| entry.id == "WebShareBuilder::ttl")
            .expect("web-share service mapping");
        entry.kind = SourceMappingKind::Descriptor;
        entry.command = Some("backend.describe".to_owned());
        entry.reason = None;
        entry.service = None;

        let error =
            CanonicalCatalog::from_documents(document, source_manifests, service_required_document)
                .expect_err("reclassified service mapping must fail completeness");

        assert!(
            error
                .to_string()
                .contains("source service mappings do not match the pinned inventory")
        );
    }

    #[test]
    fn rmux_source_mappings_preserve_operation_semantics() {
        let catalog = canonical_catalog();

        for (manifest, source_operation) in [
            ("rmux_requests_0_9_1", "Request::KillServer"),
            ("rmux_sdk_0_9_1", "Rmux::shutdown"),
        ] {
            let mapping = catalog
                .source_mapping(manifest, source_operation)
                .expect("RMUX shutdown source mapping");

            assert_eq!(mapping.kind, SourceMappingKind::Descriptor);
            assert_eq!(mapping.command.as_deref(), Some("instance.kill"));
            assert!(mapping.reason.is_none());
            assert!(mapping.service.is_none());
        }

        for (source_operation, reason) in [
            (
                "OwnedSession::lease_lost",
                "owner-local lease-loss state is not exposed as a Bootty command",
            ),
            (
                "OwnedSession::lease_state",
                "owner-local lease state is not exposed as a Bootty command",
            ),
            (
                "OwnedSession::lease_state_receiver",
                "owner-local lease-state watch receiver is not exposed as a Bootty command",
            ),
        ] {
            let mapping = catalog
                .source_mapping("rmux_sdk_0_9_1", source_operation)
                .expect("owned-session lease source mapping");

            assert_eq!(mapping.kind, SourceMappingKind::Unsupported);
            assert_eq!(mapping.reason.as_deref(), Some(reason));
            assert!(mapping.command.is_none());
            assert!(mapping.service.is_none());
        }
    }

    #[test]
    fn aliases_resolve_to_the_canonical_descriptor() {
        let catalog = canonical_catalog();

        assert_eq!(catalog.canonical_id("new-session"), Some("session.create"));
        assert_eq!(
            catalog
                .descriptor("new-session")
                .map(|descriptor| descriptor.id.as_str()),
            Some("session.create")
        );
    }

    #[test]
    fn session_create_and_parameterized_search_aliases_are_truthful() {
        let catalog = canonical_catalog();
        let descriptor = catalog
            .descriptor("session.create")
            .expect("session.create descriptor");

        assert_eq!(
            descriptor.aliases,
            vec!["new-session".to_owned(), "new_mux_session".to_owned()]
        );
        assert_eq!(descriptor.argument_schema.len(), 1);
        assert_eq!(descriptor.argument_schema[0].name, "launch");
        assert_eq!(
            descriptor.argument_schema[0].value_type,
            CatalogValueType::Object
        );
        assert!(descriptor.argument_schema[0].required);
        assert_eq!(
            descriptor.result_schema.value_type,
            CatalogValueType::Object
        );
        assert_eq!(descriptor.targets, vec![CatalogTarget::Binding]);
        assert_eq!(descriptor.availability.core, CatalogAvailability::Available);
        assert_eq!(
            descriptor.availability.native,
            CatalogAvailability::Conditional
        );
        assert_eq!(
            descriptor.availability.rmux,
            CatalogAvailability::Conditional
        );
        assert_eq!(
            descriptor.availability.tmux,
            CatalogAvailability::Conditional
        );
        assert_eq!(descriptor.mutation, CatalogMutation::Write);

        assert_eq!(
            catalog.canonical_id("navigate_search:next"),
            Some("terminal.search.next")
        );
        assert_eq!(
            catalog.canonical_id("navigate_search:previous"),
            Some("terminal.search.previous")
        );
    }

    #[test]
    fn named_metadata_defects_remain_explicit() {
        let catalog = canonical_catalog();
        let neighbor = catalog.descriptor("pane.neighbor").expect("pane.neighbor");
        let zoom = catalog.descriptor("pane.zoom").expect("pane.zoom");
        let scroll = catalog
            .descriptor("terminal.scroll.lines")
            .expect("terminal.scroll.lines");
        let messages = catalog
            .descriptor("server.messages")
            .expect("server.messages");
        let snapshot = catalog
            .descriptor("event.snapshot")
            .expect("event.snapshot");
        let rebase = catalog.descriptor("event.rebase").expect("event.rebase");

        assert_eq!(neighbor.mutation, CatalogMutation::Read);
        assert_eq!(neighbor.availability.rmux, CatalogAvailability::Unsupported);
        assert_eq!(zoom.availability.native, CatalogAvailability::Unsupported);
        assert_eq!(neighbor.argument_schema.len(), 1);
        assert_eq!(neighbor.argument_schema[0].name, "direction");
        assert_eq!(
            neighbor.argument_schema[0].value_type,
            CatalogValueType::Enum
        );
        assert_eq!(
            neighbor.argument_schema[0].choices,
            vec![
                "left".to_owned(),
                "right".to_owned(),
                "up".to_owned(),
                "down".to_owned()
            ]
        );
        assert_eq!(messages.mutation, CatalogMutation::Read);
        assert_eq!(scroll.argument_schema.len(), 1);
        assert_eq!(scroll.argument_schema[0].name, "delta");
        assert_eq!(
            scroll.argument_schema[0].value_type,
            CatalogValueType::Integer
        );
        assert_eq!(scroll.argument_schema[0].minimum, Some(i64::from(i16::MIN)));
        assert_eq!(scroll.argument_schema[0].maximum, Some(i64::from(i16::MAX)));
        for descriptor in [snapshot, rebase] {
            assert_eq!(descriptor.argument_schema.len(), 1);
            assert_eq!(
                descriptor.argument_schema[0].value_type,
                CatalogValueType::ResourceRef
            );
            assert!(descriptor.argument_schema[0].required);
            assert_eq!(descriptor.availability.core, CatalogAvailability::Available);
        }
        assert_eq!(
            catalog
                .source_mapping("control_plane", "event.snapshot")
                .map(|mapping| mapping.command.as_deref()),
            Some(Some("event.snapshot"))
        );
        assert_eq!(
            catalog
                .source_mapping("control_plane", "event.rebase")
                .map(|mapping| mapping.command.as_deref()),
            Some(Some("event.rebase"))
        );
    }

    #[test]
    fn live_source_declarations_have_explicit_mappings() {
        let catalog = canonical_catalog();
        let registry = CommandRegistry::core();

        for action in SidebarAction::ALL {
            let source_id = action.command_id();
            let mapping = catalog
                .source_mapping("bootty_actions", source_id)
                .unwrap_or_else(|| {
                    panic!("missing source mapping for live sidebar action {source_id}")
                });
            let canonical = catalog.canonical_id(source_id).unwrap_or_else(|| {
                panic!("missing catalog alias for live sidebar action {source_id}")
            });

            assert_eq!(mapping.kind, SourceMappingKind::Alias);
            assert_eq!(mapping.command.as_deref(), Some(canonical));
            assert_eq!(
                registry.describe(source_id).map(|descriptor| descriptor.id),
                Some(canonical.to_owned()),
                "live sidebar action {source_id} is not registry-runnable"
            );
        }

        {
            let (alias, canonical) = ("terminal.write", "terminal.send_text");
            let mapping = catalog
                .source_mapping("bootty_actions", alias)
                .unwrap_or_else(|| panic!("missing source mapping for registry alias {alias}"));

            assert_eq!(mapping.kind, SourceMappingKind::Alias);
            assert_eq!(mapping.command.as_deref(), Some(canonical));
            assert_eq!(
                registry.describe(alias).map(|descriptor| descriptor.id),
                Some(canonical.to_owned()),
                "registry-injected alias {alias} is not runnable"
            );
        }

        for (manifest, inventory) in &catalog.document.source_manifest_inventory {
            for (source_operation, service) in &inventory.service_required {
                let mapping = catalog
                    .source_mapping(manifest, source_operation)
                    .unwrap_or_else(|| {
                        panic!("missing service source mapping {manifest}:{source_operation}")
                    });

                assert_eq!(mapping.kind, SourceMappingKind::ServiceRequired);
                assert_eq!(mapping.service.as_deref(), Some(service.as_str()));
                assert!(mapping.command.is_none());
                assert!(mapping.reason.is_none());
            }
        }
        assert!(catalog.service_required("rmux.web-share").is_some());
    }

    #[test]
    fn agent_commands_are_unavailable_extension_placeholders() {
        let descriptor = canonical_catalog()
            .descriptor("agents.start")
            .expect("agent descriptor");

        assert!(matches!(
            &descriptor.origin,
            CatalogOrigin::Extension { extension_id, .. } if extension_id == "bootty.agents"
        ));
        assert_eq!(
            descriptor.availability.core,
            CatalogAvailability::Unavailable
        );
    }
}
