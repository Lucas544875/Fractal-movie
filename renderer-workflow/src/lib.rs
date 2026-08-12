//! Stateful workflow services for agent-driven fractal production.

mod artifact;
mod encode;
mod jobs;
mod preview;
mod project;
mod render;
mod route;

pub use artifact::{Artifact, ImageMetrics, RenderEnvironment};
pub use encode::{EncodeRequest, EncodeResult, SequenceSelection, VideoOverrides};
pub use jobs::{Harness, JobKind, JobManifest, JobProgress, JobStatus, ResourceBudget, ToolError};
pub use preview::{
    PreviewFrameResult, PreviewProfile, PreviewRequest, PreviewResult, render_preview,
};
pub use project::{
    ChangeCost, ParameterDescriptor, PatchOperation, Project, ProjectStore, Revision, SceneDocument,
};
pub use render::{
    RenderRequest, RenderResult, SequenceManifest, read_sequence_manifest, render_sequence,
};
pub use route::{
    RouteInspectRequest, RouteInspection, RouteSample, TargetCandidate, TargetSearchConfigReport,
    TargetSearchMode, TargetSearchRequest, TargetSearchResult, inspect_route, search_targets,
};
