//! Stateful workflow services for agent-driven fractal production.

mod artifact;
mod encode;
mod execution;
mod jobs;
mod preview;
mod project;
mod render;
mod route;
mod video;

pub use artifact::{Artifact, ImageMetrics, RenderEnvironment};
pub use encode::{EncodeRequest, EncodeResult, SequenceSelection, VideoOverrides};
pub use execution::{
    FrameRenderSession, FrameRenderStart, FrameRenderSummary, RenderedFrame, RendererPolicy,
    sample_scene_frame,
};
pub use jobs::{Harness, JobKind, JobManifest, JobProgress, JobStatus, ResourceBudget, ToolError};
pub use preview::{
    PreviewFrameResult, PreviewProfile, PreviewRequest, PreviewResult, apply_preview_profile,
    normalized_render_region, preview_frame_indices, render_preview,
};
pub use project::{
    ChangeCost, ParameterDescriptor, PatchOperation, Project, ProjectStore, Revision, SceneDocument,
};
pub use render::{
    RenderRequest, RenderResult, SequenceManifest, frame_path as sequence_frame_path,
    read_sequence_manifest, render_sequence, validate_png as validate_sequence_png,
};
pub use route::{
    RouteInspectRequest, RouteInspection, RouteSample, TargetCandidate, TargetSearchConfigReport,
    TargetSearchMode, TargetSearchRequest, TargetSearchResult, inspect_route, search_targets,
};
pub use video::{VideoEncodeJob, VideoEncodeSummary};
