/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Normalized deterministic-time and settlement policy for one Pliego render request.
//!
//! These values mirror the controlled runtime policy. API 2 only advertises the subset enforced by
//! the controlled clock or host-wall deadline; API 1 recovery identity retains the additional
//! internal safety fields until their enforcement seam is complete.

use std::fmt;
use std::time::Duration;

use embedder_traits::{DocumentClockConfiguration, DocumentExecutionLimits, DocumentUnixTime};

pub const API2_TIME_POLICY_VERSION: u8 = 1;
pub const API2_SETTLEMENT_POLICY_VERSION: u8 = 1;
pub const API2_EMPTY_CHECKPOINTS: u8 = 2;
pub const API2_EPOCH_LIMIT_MS: i64 = 8_640_000_000_000_000;
pub const API2_VIRTUAL_SPAN_MAX_MS: u64 = 9_007_199_254_740_991;

const NANOSECONDS_PER_MILLISECOND: u128 = 1_000_000;

/// The only version-1 disposition for an unbounded settlement source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InfiniteSourcePolicy {
    Fail,
}

/// Normalized document-observable clock policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentTimePolicy {
    pub epoch_unix_ms: i64,
    pub initial_offset_ns: u128,
}

/// Every version-1 convergence and host-safety limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentSettlementLimits {
    pub virtual_span_ms: u64,
    pub ordinary_tasks: u64,
    pub microtasks: u64,
    pub rendering_opportunities: u64,
    pub mutations: u64,
    pub post_readiness_resources: u64,
    pub process_cpu_ms: u64,
    pub host_wall_ms: u64,
}

/// Normalized fail-closed settlement policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentSettlementPolicy {
    pub infinite_source_policy: InfiniteSourcePolicy,
    pub empty_checkpoints: u8,
    pub limits: DocumentSettlementLimits,
}

/// Full deterministic runtime policy bound to a render request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeterministicRuntimePolicy {
    pub time: DocumentTimePolicy,
    pub settlement: DocumentSettlementPolicy,
}

impl Default for DeterministicRuntimePolicy {
    fn default() -> Self {
        Self {
            time: DocumentTimePolicy {
                epoch_unix_ms: 946_684_800_000,
                initial_offset_ns: 0,
            },
            settlement: DocumentSettlementPolicy {
                infinite_source_policy: InfiniteSourcePolicy::Fail,
                empty_checkpoints: API2_EMPTY_CHECKPOINTS,
                limits: DocumentSettlementLimits {
                    virtual_span_ms: 86_400_000,
                    ordinary_tasks: 100_000,
                    microtasks: 1_000_000,
                    rendering_opportunities: 10_000,
                    mutations: 1_000_000,
                    post_readiness_resources: 1_024,
                    process_cpu_ms: 30_000,
                    host_wall_ms: 60_000,
                },
            },
        }
    }
}

impl DeterministicRuntimePolicy {
    /// Validate the exact API 2 version-1 numeric and fixed-policy domain.
    pub fn validate(self) -> Result<Self, RuntimePolicyError> {
        if !(-API2_EPOCH_LIMIT_MS..=API2_EPOCH_LIMIT_MS).contains(&self.time.epoch_unix_ms) {
            return Err(RuntimePolicyError::EpochOutsideContract);
        }
        if self.time.initial_offset_ns != 0 {
            return Err(RuntimePolicyError::InitialOffsetUnsupported);
        }
        if self.settlement.infinite_source_policy != InfiniteSourcePolicy::Fail {
            return Err(RuntimePolicyError::InfiniteSourcePolicyUnsupported);
        }
        if self.settlement.empty_checkpoints != API2_EMPTY_CHECKPOINTS {
            return Err(RuntimePolicyError::EmptyCheckpointCountUnsupported);
        }

        let limits = self.settlement.limits;
        if !(1..=API2_VIRTUAL_SPAN_MAX_MS).contains(&limits.virtual_span_ms) {
            return Err(RuntimePolicyError::VirtualSpanOutsideContract);
        }
        for (name, value, allow_zero) in [
            ("ordinary_tasks", limits.ordinary_tasks, false),
            ("microtasks", limits.microtasks, false),
            (
                "rendering_opportunities",
                limits.rendering_opportunities,
                false,
            ),
            ("mutations", limits.mutations, false),
            (
                "post_readiness_resources",
                limits.post_readiness_resources,
                true,
            ),
            ("process_cpu_ms", limits.process_cpu_ms, false),
            ("host_wall_ms", limits.host_wall_ms, false),
        ] {
            if (!allow_zero && value == 0) || value > u64::from(u32::MAX) {
                return Err(RuntimePolicyError::LimitOutsideContract(name));
            }
        }

        // Prove all later configuration conversions before navigation can begin.
        let _ = self.document_clock_configuration()?;
        Ok(self)
    }

    /// Build the controlled Servo clock installed before the initial navigation.
    pub fn document_clock_configuration(
        self,
    ) -> Result<DocumentClockConfiguration, RuntimePolicyError> {
        let epoch_ns = i128::from(self.time.epoch_unix_ms)
            .checked_mul(NANOSECONDS_PER_MILLISECOND as i128)
            .ok_or(RuntimePolicyError::ClockConversionOverflow)?;
        let virtual_span_ns = u128::from(self.settlement.limits.virtual_span_ms)
            .checked_mul(NANOSECONDS_PER_MILLISECOND)
            .ok_or(RuntimePolicyError::ClockConversionOverflow)?;

        Ok(DocumentClockConfiguration::Controlled {
            initial_time_ns: self.time.initial_offset_ns,
            unix_time_origin_ns: DocumentUnixTime::from_nanos(epoch_ns),
            execution_limits: Some(DocumentExecutionLimits {
                ordinary_tasks: self.settlement.limits.ordinary_tasks,
                microtasks: self.settlement.limits.microtasks,
                rendering_opportunities: self.settlement.limits.rendering_opportunities,
                mutations: self.settlement.limits.mutations,
                virtual_span: Some(timers::DocumentTime::from_nanos(virtual_span_ns)),
            }),
        })
    }

    /// Return the normalized host-wall safety limit.
    pub fn host_wall_duration(self) -> Duration {
        Duration::from_millis(self.settlement.limits.host_wall_ms)
    }
}

/// Validation failure for a normalized deterministic runtime policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimePolicyError {
    EpochOutsideContract,
    InitialOffsetUnsupported,
    InfiniteSourcePolicyUnsupported,
    EmptyCheckpointCountUnsupported,
    VirtualSpanOutsideContract,
    LimitOutsideContract(&'static str),
    ClockConversionOverflow,
}

impl fmt::Display for RuntimePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EpochOutsideContract => formatter.write_str("controlled epoch is outside API 2"),
            Self::InitialOffsetUnsupported => {
                formatter.write_str("API 2 version 1 requires initial_offset_ns = 0")
            },
            Self::InfiniteSourcePolicyUnsupported => {
                formatter.write_str("API 2 version 1 requires infinite sources to fail")
            },
            Self::EmptyCheckpointCountUnsupported => {
                formatter.write_str("API 2 version 1 requires exactly two empty checkpoints")
            },
            Self::VirtualSpanOutsideContract => {
                formatter.write_str("virtual_span_ms is outside API 2")
            },
            Self::LimitOutsideContract(name) => {
                write!(formatter, "{name} is outside API 2")
            },
            Self::ClockConversionOverflow => {
                formatter.write_str("deterministic clock conversion overflowed")
            },
        }
    }
}

impl std::error::Error for RuntimePolicyError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api2_defaults_convert_exactly_to_the_controlled_clock() {
        let policy = DeterministicRuntimePolicy::default().validate().unwrap();
        assert_eq!(
            policy.document_clock_configuration().unwrap(),
            DocumentClockConfiguration::Controlled {
                initial_time_ns: 0,
                unix_time_origin_ns: DocumentUnixTime::from_nanos(946_684_800_000_000_000),
                execution_limits: Some(DocumentExecutionLimits::API2_V1_DEFAULT),
            }
        );
        assert_eq!(policy.host_wall_duration(), Duration::from_secs(60));
    }

    #[test]
    fn accepted_epoch_and_span_boundaries_convert_without_narrowing() {
        for epoch_unix_ms in [-API2_EPOCH_LIMIT_MS, API2_EPOCH_LIMIT_MS] {
            let mut policy = DeterministicRuntimePolicy::default();
            policy.time.epoch_unix_ms = epoch_unix_ms;
            policy.settlement.limits.virtual_span_ms = API2_VIRTUAL_SPAN_MAX_MS;
            let configuration = policy
                .validate()
                .unwrap()
                .document_clock_configuration()
                .unwrap();
            let DocumentClockConfiguration::Controlled {
                unix_time_origin_ns,
                execution_limits: Some(execution_limits),
                ..
            } = configuration
            else {
                panic!("validated policy must produce a controlled clock")
            };
            assert_eq!(
                unix_time_origin_ns.as_nanos(),
                i128::from(epoch_unix_ms) * 1_000_000
            );
            assert_eq!(
                execution_limits.virtual_span.unwrap().as_nanos(),
                u128::from(API2_VIRTUAL_SPAN_MAX_MS) * 1_000_000
            );
        }
    }

    #[test]
    fn invalid_policy_is_rejected_before_clock_construction() {
        let mut policy = DeterministicRuntimePolicy::default();
        policy.time.epoch_unix_ms = API2_EPOCH_LIMIT_MS + 1;
        assert_eq!(
            policy.validate(),
            Err(RuntimePolicyError::EpochOutsideContract)
        );

        let mut policy = DeterministicRuntimePolicy::default();
        policy.settlement.limits.ordinary_tasks = 0;
        assert_eq!(
            policy.validate(),
            Err(RuntimePolicyError::LimitOutsideContract("ordinary_tasks"))
        );

        let mut policy = DeterministicRuntimePolicy::default();
        policy.settlement.limits.virtual_span_ms = API2_VIRTUAL_SPAN_MAX_MS + 1;
        assert_eq!(
            policy.validate(),
            Err(RuntimePolicyError::VirtualSpanOutsideContract)
        );
    }
}
