//! Store-free binary scanner and nested core-resource policy.
use wasmparser::{
    Encoding, FuncValidatorAllocations, MemoryType, Parser, Payload, TableType, TypeRef,
    ValidPayload, Validator, WasmFeatures,
};

use crate::component_world::ComponentWorld;
use crate::error::{ImportCategory, PreflightError};

const MAX_MEMORY_PAGES: u64 = 1_024;
const MAX_MEMORY_COUNT: u32 = 2;
const MAX_MEMORY_AGGREGATE_PAGES: u64 = 2_048;
const MAX_TABLE_ELEMENTS: u64 = 65_536;
const MAX_TABLE_COUNT: u32 = 4;
const MAX_TABLE_AGGREGATE_ELEMENTS: u64 = 65_536;
const MAX_CORE_INSTANCES: u32 = 64;
const CORE_AMBIENT_MARKERS: [&str; 24] = [
    "wasi",
    "filesystem",
    "file-system",
    "socket",
    "network",
    "dns",
    "http",
    "random",
    "clock",
    "secret",
    "credential",
    "keyvalue",
    "key-value",
    "logging",
    "log",
    "ui",
    "terminal",
    "stdin",
    "stdout",
    "render",
    "raw",
    "handle",
    "process",
    "environment",
];

pub(crate) struct ScannedComponent<'a> {
    bytes: &'a [u8],
    world: ComponentWorld,
}

impl<'a> ScannedComponent<'a> {
    pub(crate) const fn into_parts(self) -> (&'a [u8], ComponentWorld) {
        (self.bytes, self.world)
    }
}

#[derive(Default)]
struct ResourceTotals {
    memory_count: u32,
    memory_max_pages: u64,
    table_count: u32,
    table_max_elements: u64,
    core_instances: u32,
}

pub(crate) fn scan_component(
    bytes: &[u8],
    world: ComponentWorld,
) -> Result<ScannedComponent<'_>, PreflightError> {
    let features = restricted_features();
    let mut parser = Parser::new(0);
    parser.set_features(features);
    let mut validator = Validator::new_with_features(features);
    let mut function_allocations = FuncValidatorAllocations::default();
    let mut stack = Vec::new();
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    let mut totals = ResourceTotals::default();

    for payload in parser.parse_all(bytes) {
        let payload = payload.map_err(validation_error)?;
        if let ValidPayload::Func(function, body) =
            validator.payload(&payload).map_err(validation_error)?
        {
            let mut function_validator = function.into_validator(function_allocations);
            function_validator
                .validate(&body)
                .map_err(validation_error)?;
            function_allocations = function_validator.into_allocations();
        }
        if let Payload::Version { encoding, .. } = payload {
            stack.push(encoding);
            continue;
        }
        let at_root = stack.as_slice() == [Encoding::Component];
        match payload {
            Payload::ImportSection(section) => {
                for import in section.into_imports() {
                    let import = import.map_err(|_| PreflightError::InvalidComponent)?;
                    scan_core_import(import.module, import.name, import.ty, &mut totals)?;
                }
            }
            Payload::MemorySection(section) => {
                for memory in section {
                    scan_memory(
                        memory.map_err(|_| PreflightError::InvalidComponent)?,
                        &mut totals,
                    )?;
                }
            }
            Payload::TableSection(section) => {
                for table in section {
                    let table = table.map_err(|_| PreflightError::InvalidComponent)?;
                    scan_table(table.ty, &mut totals)?;
                }
            }
            Payload::InstanceSection(section) => {
                totals.core_instances = totals
                    .core_instances
                    .checked_add(section.count())
                    .ok_or(PreflightError::CoreInstanceLimit)?;
                if totals.core_instances > MAX_CORE_INSTANCES {
                    return Err(PreflightError::CoreInstanceLimit);
                }
            }
            Payload::ComponentImportSection(section) => {
                for import in section {
                    let import = import.map_err(|_| PreflightError::InvalidComponent)?;
                    let name = import.name.name;
                    if let Some(category) = component_import_category(name) {
                        return Err(PreflightError::DeniedImport(category));
                    }
                    if at_root {
                        imports.push(name);
                    }
                }
            }
            Payload::ComponentExportSection(section) => {
                if at_root {
                    for export in section {
                        exports.push(
                            export
                                .map_err(|_| PreflightError::InvalidComponent)?
                                .name
                                .name,
                        );
                    }
                }
            }
            Payload::End(_) => {
                stack.pop().ok_or(PreflightError::InvalidComponent)?;
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(PreflightError::InvalidComponent);
    }
    validate_topology(&imports, &exports, world)?;
    Ok(ScannedComponent { bytes, world })
}

fn restricted_features() -> WasmFeatures {
    let mut features = WasmFeatures::default();
    features.remove(
        WasmFeatures::MEMORY64
            | WasmFeatures::THREADS
            | WasmFeatures::SHARED_EVERYTHING_THREADS
            | WasmFeatures::CM_ASYNC
            | WasmFeatures::CM_THREADING
            | WasmFeatures::CM64,
    );
    features
}

fn validation_error(error: wasmparser::BinaryReaderError) -> PreflightError {
    if error.missing_wasm_feature().is_some() {
        PreflightError::DisabledFeature
    } else {
        PreflightError::InvalidComponent
    }
}

fn scan_core_import(
    module: &str,
    name: &str,
    ty: TypeRef,
    totals: &mut ResourceTotals,
) -> Result<(), PreflightError> {
    if let Some(category) = core_module_category(module) {
        return Err(PreflightError::DeniedImport(category));
    }
    if let Some(category) = core_module_category(name) {
        return Err(PreflightError::DeniedImport(category));
    }
    match ty {
        TypeRef::Memory(memory) => scan_memory(memory, totals),
        TypeRef::Table(table) => scan_table(table, totals),
        _ => Ok(()),
    }
}

fn scan_memory(memory: MemoryType, totals: &mut ResourceTotals) -> Result<(), PreflightError> {
    if memory.memory64 || memory.shared {
        return Err(PreflightError::DisabledFeature);
    }
    let maximum = memory.maximum.ok_or(PreflightError::UnboundedMemory)?;
    if maximum > MAX_MEMORY_PAGES {
        return Err(PreflightError::MemoryLimit);
    }
    let aggregate = totals
        .memory_max_pages
        .checked_add(maximum)
        .ok_or(PreflightError::MemoryAggregateLimit)?;
    if aggregate > MAX_MEMORY_AGGREGATE_PAGES {
        return Err(PreflightError::MemoryAggregateLimit);
    }
    let count = totals
        .memory_count
        .checked_add(1)
        .ok_or(PreflightError::MemoryCount)?;
    if count > MAX_MEMORY_COUNT {
        return Err(PreflightError::MemoryCount);
    }
    totals.memory_max_pages = aggregate;
    totals.memory_count = count;
    Ok(())
}

fn scan_table(table: TableType, totals: &mut ResourceTotals) -> Result<(), PreflightError> {
    if table.table64 || table.shared {
        return Err(PreflightError::DisabledFeature);
    }
    let maximum = table.maximum.ok_or(PreflightError::UnboundedTable)?;
    if maximum > MAX_TABLE_ELEMENTS {
        return Err(PreflightError::TableLimit);
    }
    let aggregate = totals
        .table_max_elements
        .checked_add(maximum)
        .ok_or(PreflightError::TableAggregateLimit)?;
    if aggregate > MAX_TABLE_AGGREGATE_ELEMENTS {
        return Err(PreflightError::TableAggregateLimit);
    }
    let count = totals
        .table_count
        .checked_add(1)
        .ok_or(PreflightError::TableCount)?;
    if count > MAX_TABLE_COUNT {
        return Err(PreflightError::TableCount);
    }
    totals.table_max_elements = aggregate;
    totals.table_count = count;
    Ok(())
}

fn validate_topology(
    imports: &[&str],
    exports: &[&str],
    world: ComponentWorld,
) -> Result<(), PreflightError> {
    let expected_imports = world.imports();
    if imports.len() < expected_imports.len() {
        return Err(PreflightError::MissingImport);
    }
    if imports.len() != expected_imports.len() {
        let extra = imports
            .iter()
            .find(|name| !expected_imports.contains(name))
            .copied()
            .unwrap_or_default();
        return Err(PreflightError::DeniedImport(classify_import(extra)));
    }
    if let Some(name) = imports.iter().find(|name| !expected_imports.contains(name)) {
        return Err(PreflightError::DeniedImport(classify_import(name)));
    }

    let expected_exports = world.exports();
    if exports.len() < expected_exports.len() {
        return Err(PreflightError::MissingExport);
    }
    if exports.len() != expected_exports.len()
        || exports.iter().any(|name| !expected_exports.contains(name))
    {
        return Err(PreflightError::UnexpectedExport);
    }
    Ok(())
}

pub(crate) fn classify_import(name: &str) -> ImportCategory {
    ambient_category(name).unwrap_or_else(|| {
        if name.starts_with("mcode:") {
            ImportCategory::MCodeVersion
        } else {
            ImportCategory::Extra
        }
    })
}

fn component_import_category(name: &str) -> Option<ImportCategory> {
    if name.starts_with("mcode:") {
        return None;
    }
    let name = name.to_ascii_lowercase();
    if name.contains(':') || name.starts_with("wasi_") || name.starts_with("wasi-") || name == "env"
    {
        ambient_category(&name)
    } else {
        None
    }
}

fn core_module_category(name: &str) -> Option<ImportCategory> {
    if name.starts_with("mcode:") {
        return None;
    }
    let name = name.to_ascii_lowercase();
    let is_ambient = name == "env"
        || CORE_AMBIENT_MARKERS
            .iter()
            .any(|marker| has_ambient_marker(&name, marker));
    if is_ambient {
        ambient_category(&name)
    } else {
        None
    }
}

fn has_ambient_marker(name: &str, marker: &str) -> bool {
    name.starts_with(marker) || has_import_segment(name, marker)
}

fn has_import_segment(name: &str, marker: &str) -> bool {
    name.split([':', '/']).any(|segment| {
        segment == marker
            || segment
                .strip_prefix(marker)
                .is_some_and(|suffix| suffix.starts_with('@'))
    })
}

fn ambient_category(name: &str) -> Option<ImportCategory> {
    let name = name.to_ascii_lowercase();
    let category = if name.contains("terminal") || name.contains("stdin") || name.contains("stdout")
    {
        ImportCategory::Terminal
    } else if name.contains("filesystem") || name.contains("file-system") {
        ImportCategory::Filesystem
    } else if name.contains("http") {
        ImportCategory::Http
    } else if name.contains("socket") || name.contains("network") || name.contains("dns") {
        ImportCategory::Network
    } else if name.contains("random") {
        ImportCategory::Random
    } else if name.contains("clock") {
        ImportCategory::Clocks
    } else if name.contains("secret")
        || name.contains("credential")
        || name.contains("keyvalue")
        || name.contains("key-value")
    {
        ImportCategory::Secret
    } else if name.contains("logging") || has_import_segment(&name, "log") {
        ImportCategory::Logging
    } else if has_import_segment(&name, "ui") || name.contains("render") {
        ImportCategory::UserInterface
    } else if name.contains("raw") || name.contains("handle") {
        ImportCategory::RawHost
    } else if name.contains("process") || name.contains("environment") || name.contains(":cli/") {
        ImportCategory::Process
    } else if name.starts_with("wasi:")
        || name.starts_with("wasi_")
        || name.starts_with("wasi-")
        || name.split([':', '/']).any(|segment| {
            segment == "wasi" || segment.starts_with("wasi_") || segment.starts_with("wasi-")
        })
        || name == "env"
    {
        ImportCategory::Wasi
    } else {
        return None;
    };
    Some(category)
}
