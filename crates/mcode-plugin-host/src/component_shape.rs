//! Exact Store-free comparison against canonical Wasmtime component types.
use wasmtime::Engine;
use wasmtime::component::Component;
use wasmtime::component::types::{
    Component as ComponentType, ComponentFunc, ComponentInstance, ComponentItem, ResourceType, Type,
};

use crate::component_world::ComponentWorld;
use crate::error::PreflightError;

#[derive(Default)]
struct ShapeContext {
    resources: Vec<(ResourceType, ResourceType)>,
}

pub(crate) fn validate_shape(
    candidate: &Component,
    reference: &Component,
    world: ComponentWorld,
) -> Result<(), PreflightError> {
    let candidate_type = candidate.component_type();
    let reference_type = reference.component_type();
    let engine = candidate.engine();
    let mut context = ShapeContext::default();

    compare_root_imports(
        &candidate_type,
        engine,
        &reference_type,
        world,
        &mut context,
    )?;
    compare_root_exports(
        &candidate_type,
        engine,
        &reference_type,
        world,
        &mut context,
    )
}

fn compare_root_imports(
    candidate: &ComponentType,
    engine: &Engine,
    reference: &ComponentType,
    world: ComponentWorld,
    context: &mut ShapeContext,
) -> Result<(), PreflightError> {
    for name in world.imports() {
        let candidate = candidate
            .get_import(engine, name)
            .ok_or(PreflightError::MissingImport)?;
        let reference = reference
            .get_import(engine, name)
            .ok_or(PreflightError::Engine)?;
        if !compare_item(candidate.ty, engine, reference.ty, context) {
            return Err(PreflightError::ImportShape);
        }
    }
    Ok(())
}

fn compare_root_exports(
    candidate: &ComponentType,
    engine: &Engine,
    reference: &ComponentType,
    world: ComponentWorld,
    context: &mut ShapeContext,
) -> Result<(), PreflightError> {
    for name in world.exports() {
        let candidate = candidate
            .get_export(engine, name)
            .ok_or(PreflightError::MissingExport)?;
        let reference = reference
            .get_export(engine, name)
            .ok_or(PreflightError::Engine)?;
        if !compare_item(candidate.ty, engine, reference.ty, context) {
            return Err(PreflightError::ExportShape);
        }
    }
    Ok(())
}

fn compare_item(
    candidate: ComponentItem,
    engine: &Engine,
    reference: ComponentItem,
    context: &mut ShapeContext,
) -> bool {
    match (candidate, reference) {
        (
            ComponentItem::ComponentInstance(candidate),
            ComponentItem::ComponentInstance(reference),
        ) => compare_instance(&candidate, engine, &reference, context),
        (ComponentItem::ComponentFunc(candidate), ComponentItem::ComponentFunc(reference)) => {
            compare_function(&candidate, &reference, context)
        }
        (ComponentItem::Type(candidate), ComponentItem::Type(reference)) => {
            compare_type(&candidate, &reference, context)
        }
        (ComponentItem::Resource(candidate), ComponentItem::Resource(reference)) => {
            remember_resource(candidate, reference, context)
        }
        _ => false,
    }
}

fn compare_instance(
    candidate: &ComponentInstance,
    engine: &Engine,
    reference: &ComponentInstance,
    context: &mut ShapeContext,
) -> bool {
    let candidate: Vec<_> = candidate.exports(engine).collect();
    let reference: Vec<_> = reference.exports(engine).collect();
    if candidate.len() != reference.len() {
        return false;
    }

    for ((candidate_name, candidate_item), (reference_name, reference_item)) in
        candidate.iter().zip(&reference)
    {
        if candidate_name != reference_name {
            return false;
        }
        match (&candidate_item.ty, &reference_item.ty) {
            (
                ComponentItem::Resource(candidate_resource),
                ComponentItem::Resource(reference_resource),
            ) => {
                if !remember_resource(*candidate_resource, *reference_resource, context) {
                    return false;
                }
            }
            (ComponentItem::Resource(_), _) | (_, ComponentItem::Resource(_)) => return false,
            _ => {}
        }
    }

    candidate
        .iter()
        .zip(&reference)
        .all(|((_, candidate_item), (_, reference_item))| {
            compare_item(
                candidate_item.ty.clone(),
                engine,
                reference_item.ty.clone(),
                context,
            )
        })
}

fn remember_resource(
    candidate: ResourceType,
    reference: ResourceType,
    context: &mut ShapeContext,
) -> bool {
    if let Some((_, known_reference)) = context
        .resources
        .iter()
        .find(|(known_candidate, _)| *known_candidate == candidate)
    {
        return *known_reference == reference;
    }
    if context
        .resources
        .iter()
        .any(|(_, known_reference)| *known_reference == reference)
    {
        return false;
    }
    context.resources.push((candidate, reference));
    true
}

fn compare_function(
    candidate: &ComponentFunc,
    reference: &ComponentFunc,
    context: &ShapeContext,
) -> bool {
    if candidate.async_() != reference.async_() {
        return false;
    }
    let candidate_params: Vec<_> = candidate.params().collect();
    let reference_params: Vec<_> = reference.params().collect();
    if candidate_params.len() != reference_params.len()
        || !candidate_params.iter().zip(&reference_params).all(
            |((candidate_name, candidate_type), (reference_name, reference_type))| {
                candidate_name == reference_name
                    && compare_type(candidate_type, reference_type, context)
            },
        )
    {
        return false;
    }
    let candidate_results: Vec<_> = candidate.results().collect();
    let reference_results: Vec<_> = reference.results().collect();
    candidate_results.len() == reference_results.len()
        && candidate_results
            .iter()
            .zip(&reference_results)
            .all(|(candidate, reference)| compare_type(candidate, reference, context))
}

fn compare_type(candidate: &Type, reference: &Type, context: &ShapeContext) -> bool {
    match (candidate, reference) {
        (Type::List(candidate), Type::List(reference)) => {
            compare_type(&candidate.ty(), &reference.ty(), context)
        }
        (Type::Record(candidate), Type::Record(reference)) => {
            let candidate: Vec<_> = candidate.fields().collect();
            let reference: Vec<_> = reference.fields().collect();
            candidate.len() == reference.len()
                && candidate
                    .iter()
                    .zip(&reference)
                    .all(|(candidate, reference)| {
                        candidate.name == reference.name
                            && compare_type(&candidate.ty, &reference.ty, context)
                    })
        }
        (Type::Tuple(candidate), Type::Tuple(reference)) => {
            let candidate: Vec<_> = candidate.types().collect();
            let reference: Vec<_> = reference.types().collect();
            candidate.len() == reference.len()
                && candidate
                    .iter()
                    .zip(&reference)
                    .all(|(candidate, reference)| compare_type(candidate, reference, context))
        }
        (Type::Variant(candidate), Type::Variant(reference)) => {
            let candidate: Vec<_> = candidate.cases().collect();
            let reference: Vec<_> = reference.cases().collect();
            candidate.len() == reference.len()
                && candidate
                    .iter()
                    .zip(&reference)
                    .all(|(candidate, reference)| {
                        candidate.name == reference.name
                            && compare_optional_type(
                                candidate.ty.as_ref(),
                                reference.ty.as_ref(),
                                context,
                            )
                    })
        }
        (Type::Enum(candidate), Type::Enum(reference)) => candidate.names().eq(reference.names()),
        (Type::Option(candidate), Type::Option(reference)) => {
            compare_type(&candidate.ty(), &reference.ty(), context)
        }
        (Type::Result(candidate), Type::Result(reference)) => {
            compare_optional_type(candidate.ok().as_ref(), reference.ok().as_ref(), context)
                && compare_optional_type(
                    candidate.err().as_ref(),
                    reference.err().as_ref(),
                    context,
                )
        }
        (Type::Flags(candidate), Type::Flags(reference)) => candidate.names().eq(reference.names()),
        (Type::Own(candidate), Type::Own(reference))
        | (Type::Borrow(candidate), Type::Borrow(reference)) => {
            context
                .resources
                .iter()
                .any(|(known_candidate, known_reference)| {
                    known_candidate == candidate && known_reference == reference
                })
        }
        _ => candidate == reference,
    }
}

fn compare_optional_type(
    candidate: Option<&Type>,
    reference: Option<&Type>,
    context: &ShapeContext,
) -> bool {
    match (candidate, reference) {
        (Some(candidate), Some(reference)) => compare_type(candidate, reference, context),
        (None, None) => true,
        _ => false,
    }
}
