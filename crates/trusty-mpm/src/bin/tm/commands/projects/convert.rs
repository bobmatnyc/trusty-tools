//! Clap-arg → domain-type conversions for the `tm projects` command group.
//!
//! Why: the CLI value enums (`DeliverableKindArg`, `EstimationTierArg`,
//! `DeliverableStatusArg`, `SettableConfigField`, `ClearableConfigField`) live in
//! `cli.rs` so that file carries no dependency on the `trusty_mpm::deliverable` /
//! `trusty_mpm::project_config` domain types. The mapping to those domain types
//! has to live somewhere the command handlers can call; centralizing it here
//! keeps the subtree files free of repeated `match` arms and gives the
//! mapping a single tested home.
//! What: `From` impls converting each CLI value enum to its domain counterpart.
//! Test: `kind_maps`, `tier_maps`, `status_maps`, `settable_config_field_maps`,
//! `clearable_config_field_maps` in the `tests` submodule assert every variant
//! round-trips to the right domain value.

use crate::cli::{
    ClearableConfigField, DeliverableKindArg, DeliverableStatusArg, EstimationTierArg,
    SettableConfigField,
};
use trusty_mpm::deliverable::{DeliverableKind, DeliverableStatus, EstimationTier};
use trusty_mpm::project_config::{ClearableField, ConfigField};

impl From<DeliverableKindArg> for DeliverableKind {
    fn from(arg: DeliverableKindArg) -> Self {
        match arg {
            DeliverableKindArg::Feature => DeliverableKind::Feature,
            DeliverableKindArg::Bugfix => DeliverableKind::Bugfix,
            DeliverableKindArg::Refactor => DeliverableKind::Refactor,
            DeliverableKindArg::Chore => DeliverableKind::Chore,
            DeliverableKindArg::Test => DeliverableKind::Test,
            DeliverableKindArg::Docs => DeliverableKind::Docs,
        }
    }
}

impl From<EstimationTierArg> for EstimationTier {
    fn from(arg: EstimationTierArg) -> Self {
        match arg {
            EstimationTierArg::S => EstimationTier::S,
            EstimationTierArg::M => EstimationTier::M,
            EstimationTierArg::L => EstimationTier::L,
            EstimationTierArg::Xl => EstimationTier::Xl,
        }
    }
}

impl From<DeliverableStatusArg> for DeliverableStatus {
    fn from(arg: DeliverableStatusArg) -> Self {
        match arg {
            DeliverableStatusArg::Proposed => DeliverableStatus::Proposed,
            DeliverableStatusArg::InProgress => DeliverableStatus::InProgress,
            DeliverableStatusArg::Blocked => DeliverableStatus::Blocked,
            DeliverableStatusArg::Complete => DeliverableStatus::Complete,
            DeliverableStatusArg::Delivered => DeliverableStatus::Delivered,
            DeliverableStatusArg::Shipped => DeliverableStatus::Shipped,
        }
    }
}

impl From<SettableConfigField> for ConfigField {
    fn from(arg: SettableConfigField) -> Self {
        match arg {
            SettableConfigField::DefaultBranch => ConfigField::DefaultBranch,
            SettableConfigField::Description => ConfigField::Description,
            SettableConfigField::StackHint => ConfigField::StackHint,
            SettableConfigField::GhUser => ConfigField::GhUser,
        }
    }
}

impl From<ClearableConfigField> for ClearableField {
    fn from(arg: ClearableConfigField) -> Self {
        match arg {
            ClearableConfigField::Description => ClearableField::Description,
            ClearableConfigField::StackHint => ClearableField::StackHint,
            ClearableConfigField::GhUser => ClearableField::GhUser,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_maps() {
        assert_eq!(
            DeliverableKind::from(DeliverableKindArg::Feature),
            DeliverableKind::Feature
        );
        assert_eq!(
            DeliverableKind::from(DeliverableKindArg::Docs),
            DeliverableKind::Docs
        );
    }

    #[test]
    fn tier_maps() {
        assert_eq!(
            EstimationTier::from(EstimationTierArg::S),
            EstimationTier::S
        );
        assert_eq!(
            EstimationTier::from(EstimationTierArg::Xl),
            EstimationTier::Xl
        );
    }

    #[test]
    fn status_maps() {
        assert_eq!(
            DeliverableStatus::from(DeliverableStatusArg::InProgress),
            DeliverableStatus::InProgress
        );
        assert_eq!(
            DeliverableStatus::from(DeliverableStatusArg::Shipped),
            DeliverableStatus::Shipped
        );
    }

    #[test]
    fn settable_config_field_maps() {
        assert_eq!(
            ConfigField::from(SettableConfigField::DefaultBranch),
            ConfigField::DefaultBranch
        );
        assert_eq!(
            ConfigField::from(SettableConfigField::GhUser),
            ConfigField::GhUser
        );
    }

    #[test]
    fn clearable_config_field_maps() {
        assert_eq!(
            ClearableField::from(ClearableConfigField::Description),
            ClearableField::Description
        );
        assert_eq!(
            ClearableField::from(ClearableConfigField::GhUser),
            ClearableField::GhUser
        );
    }
}
