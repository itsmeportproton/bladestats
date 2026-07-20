//! Hardware telemetry collection.
//!
//! Each backend reports only what it can genuinely read and leaves the rest as `None`. The
//! aggregator merges them richest-first: a vendor SDK overrides the generic source, and
//! whatever it cannot supply is filled in from below.
//!
//! Populated in stage 3.
