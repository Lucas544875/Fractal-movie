//! Production harness that lets an LLM agent design, render, and encode a
//! multi-cut fractal film. See `docs/harness-spec.md`.
//!
//! The harness owns the stage order, every gate decision, the diversity
//! constraints, and the render budget. Agents only author the brief, the shot
//! list, and the look.

pub mod animatic;
pub mod axes;
pub mod brief;
pub mod budget;
pub mod calibrate;
pub mod classify;
pub mod cut;
pub mod describe;
pub mod encode;
pub mod film;
pub mod gpu;
pub mod import;
pub mod ledger;
pub mod look;
pub mod mandate;
pub mod metrics;
pub mod next;
pub mod novelty;
pub mod produce;
pub mod production;
pub mod proof;
pub mod quality;
pub mod rng;
pub mod sample;
pub mod schema;
pub mod scout;
pub mod shape;
pub mod sheet;
pub mod shotlist;
pub mod stage;
pub mod storyboard;
pub mod tuning;
pub mod vocabulary;
