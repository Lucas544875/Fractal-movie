//! Production harness that lets an LLM agent design, render, and encode a
//! multi-cut fractal film. See `docs/harness-spec.md`.
//!
//! The harness owns the stage order, every gate decision, the diversity
//! constraints, and the render budget. Agents only author the brief, the shot
//! list, and the look.

pub mod axes;
pub mod budget;
pub mod describe;
pub mod gpu;
pub mod ledger;
pub mod mandate;
pub mod metrics;
pub mod next;
pub mod production;
pub mod quality;
pub mod rng;
pub mod scout;
pub mod shape;
pub mod sheet;
pub mod stage;
