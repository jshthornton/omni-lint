//! GraphQL rules — one module per rule.
//!
//! Every rule is an independent unit: implement `Rule`, give it an id, and
//! register just the ones you want into the ruleset (like eslint's `rules`
//! map). `all_rules()` is only the convenience bundle of the example pack.

mod deprecated_required_input;
mod duplicate_type;
mod field_name_camel;
mod no_empty_type;
mod no_undefined_type;
mod no_unused_type;
mod shared;
mod type_name_pascal;

pub use deprecated_required_input::DeprecatedRequiredInput;
pub use duplicate_type::DuplicateType;
pub use field_name_camel::FieldNameCamel;
pub use no_empty_type::NoEmptyType;
pub use no_undefined_type::NoUndefinedType;
pub use no_unused_type::NoUnusedType;
pub use type_name_pascal::TypeNamePascal;

use omni_core::Rule;
use std::sync::Arc;

/// Every example rule, in registration order. Opt-in convenience only —
/// assemble your own ruleset from individual rules instead when you prefer.
pub fn all_rules() -> Vec<Arc<dyn Rule>> {
    vec![
        Arc::new(NoEmptyType),
        Arc::new(TypeNamePascal),
        Arc::new(FieldNameCamel),
        Arc::new(DeprecatedRequiredInput),
        Arc::new(NoUndefinedType),
        Arc::new(DuplicateType),
        Arc::new(NoUnusedType),
    ]
}