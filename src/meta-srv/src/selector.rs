// Copyright 2023 Greptime Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod lease_based;
pub mod load_based;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use self::lease_based::LeaseBasedSelector;
use self::load_based::LoadBasedSelector;
use crate::error;
use crate::error::Result;
use crate::metasrv::SelectorRef;

pub type Namespace = u64;

#[async_trait::async_trait]
pub trait Selector: Send + Sync {
    type Context;
    type Output;

    async fn select(&self, ns: Namespace, ctx: &Self::Context) -> Result<Self::Output>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectorType {
    LoadBased,
    LeaseBased,
}

impl From<SelectorType> for SelectorRef {
    fn from(selector_type: SelectorType) -> Self {
        match selector_type {
            SelectorType::LoadBased => Arc::new(LoadBasedSelector) as SelectorRef,
            SelectorType::LeaseBased => Arc::new(LeaseBasedSelector) as SelectorRef,
        }
    }
}

impl Default for SelectorType {
    fn default() -> Self {
        SelectorType::LeaseBased
    }
}

impl TryFrom<&str> for SelectorType {
    type Error = error::Error;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "LoadBased" => Ok(SelectorType::LoadBased),
            "LeaseBased" => Ok(SelectorType::LeaseBased),
            other => error::UnsupportedSelectorTypeSnafu {
                selector_type: other,
            }
            .fail(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SelectorType;
    use crate::error::Result;

    #[test]
    fn test_default_selector_type() {
        assert_eq!(SelectorType::LeaseBased, SelectorType::default());
    }

    #[test]
    fn test_convert_str_to_selector_type() {
        let leasebased = "LeaseBased";
        let selector_type = leasebased.try_into().unwrap();
        assert_eq!(SelectorType::LeaseBased, selector_type);

        let loadbased = "LoadBased";
        let selector_type = loadbased.try_into().unwrap();
        assert_eq!(SelectorType::LoadBased, selector_type);

        let unknow = "unknow";
        let selector_type: Result<SelectorType> = unknow.try_into();
        assert!(selector_type.is_err());
    }
}
