//! Closed ordinary-header rule matching.
use crate::provider_validation::prepare::validate_ordinary_headers;
use crate::provider_wit::exports::mcode::provider_pack::provider_api::OrdinaryHeader;

use super::types::{AdapterValidationError, AdapterValidationResult, OrdinaryHeaderRule};

pub(super) fn validate_headers(
    rules: &[OrdinaryHeaderRule],
    headers: &[OrdinaryHeader],
) -> AdapterValidationResult<()> {
    validate_ordinary_headers(headers, &[]).map_err(|error| match error {
        crate::provider_validation::ValidationError::InvalidArgument => {
            AdapterValidationError::HeaderMismatch
        }
        crate::provider_validation::ValidationError::Limit => AdapterValidationError::Limit,
    })?;

    let mut header_index = 0;
    for rule in rules {
        match rule {
            OrdinaryHeaderRule::Fixed(rule) => {
                if headers
                    .get(header_index)
                    .is_some_and(|header| header.name == rule.name)
                {
                    return Err(AdapterValidationError::HeaderMismatch);
                }
            }
            OrdinaryHeaderRule::OneOf(rule) => {
                let matching = headers
                    .get(header_index)
                    .filter(|header| header.name == rule.name);
                match matching {
                    Some(header) if rule.values.binary_search(&header.value).is_ok() => {
                        header_index += 1;
                    }
                    Some(_) => return Err(AdapterValidationError::HeaderMismatch),
                    None if rule.required => return Err(AdapterValidationError::HeaderMismatch),
                    None => {}
                }
            }
        }
    }
    if header_index != headers.len() {
        return Err(AdapterValidationError::HeaderMismatch);
    }
    Ok(())
}
