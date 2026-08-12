use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use fractal_renderer_core::{SceneSpec, parse_scene_spec};
use serde::{Deserialize, Serialize};

use crate::artifact::{sha256_hex, write_atomic};

pub type SceneDocument = SceneSpec;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Project {
    pub version: u32,
    pub id: String,
    pub current_revision: String,
    pub created_unix_ms: u64,
    pub source_scene: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Revision {
    pub id: String,
    pub scene_hash: String,
    pub parent_revision: Option<String>,
    pub created_unix_ms: u64,
    #[serde(default)]
    pub changes: Vec<PatchOperation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum PatchOperation {
    Set {
        path: String,
        value: serde_json::Value,
    },
    Remove {
        path: String,
    },
    Insert {
        path: String,
        index: usize,
        value: serde_json::Value,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeCost {
    Metadata,
    PostProcess,
    Uniform,
    Pipeline,
    FullRender,
    Encode,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ParameterDescriptor {
    pub path: String,
    pub value_type: String,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub change_cost: ChangeCost,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct ProjectStore {
    root: PathBuf,
}

impl ProjectStore {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn create(&self, project_id: &str, source_scene: impl AsRef<Path>) -> Result<Project> {
        validate_id("project", project_id)?;
        let project_directory = self.project_directory(project_id)?;
        if project_directory.exists() {
            bail!("project {project_id} already exists");
        }
        let source_scene = source_scene.as_ref();
        let source_yaml = fs::read_to_string(source_scene)
            .with_context(|| format!("could not read source scene {}", source_scene.display()))?;
        let spec = parse_scene_spec(&source_yaml).context("source scene is invalid")?;
        let canonical_yaml = spec.to_yaml()?;
        let revision = self.write_revision(project_id, None, &canonical_yaml, Vec::new())?;
        let project = Project {
            version: 1,
            id: project_id.to_owned(),
            current_revision: revision.id,
            created_unix_ms: unix_time_ms(),
            source_scene: source_scene.to_owned(),
        };
        self.write_project(&project)?;
        fs::create_dir_all(project_directory.join("runs"))?;
        Ok(project)
    }

    pub fn project(&self, project_id: &str) -> Result<Project> {
        let path = self.project_file(project_id)?;
        let bytes = fs::read(&path)
            .with_context(|| format!("could not read project {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("project metadata {} is invalid", path.display()))
    }

    pub fn revision(&self, project_id: &str, revision_id: &str) -> Result<Revision> {
        validate_id("revision", revision_id)?;
        let path = self
            .project_directory(project_id)?
            .join("revisions")
            .join(format!("{revision_id}.json"));
        let bytes = fs::read(&path)
            .with_context(|| format!("could not read revision {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("revision metadata {} is invalid", path.display()))
    }

    pub fn scene(
        &self,
        project_id: &str,
        revision_id: Option<&str>,
    ) -> Result<(Revision, SceneSpec)> {
        let project = self.project(project_id)?;
        let revision_id = revision_id.unwrap_or(&project.current_revision);
        let revision = self.revision(project_id, revision_id)?;
        let path = self.scene_path(project_id, revision_id)?;
        let yaml = fs::read_to_string(&path)
            .with_context(|| format!("could not read revision scene {}", path.display()))?;
        let actual_hash = sha256_hex(yaml.as_bytes());
        if actual_hash != revision.scene_hash {
            bail!("revision {revision_id} scene hash does not match its immutable metadata");
        }
        let spec = parse_scene_spec(&yaml).context("stored revision is invalid")?;
        Ok((revision, spec))
    }

    pub fn patch(
        &self,
        project_id: &str,
        base_revision: &str,
        operations: &[PatchOperation],
        promote: bool,
    ) -> Result<Revision> {
        if operations.is_empty() {
            bail!("scene patch must contain at least one operation");
        }
        if operations.len() > 256 {
            bail!("scene patch must not contain more than 256 operations");
        }
        if operations.iter().any(|operation| match operation {
            PatchOperation::Set { path, .. }
            | PatchOperation::Remove { path }
            | PatchOperation::Insert { path, .. } => path.len() > 512,
        }) {
            bail!("scene patch paths must not exceed 512 bytes");
        }
        let mut project = self.project(project_id)?;
        if promote && project.current_revision != base_revision {
            bail!(
                "revision conflict: project current revision is {}, not {base_revision}",
                project.current_revision
            );
        }
        let (_, spec) = self.scene(project_id, Some(base_revision))?;
        let mut document = serde_json::to_value(spec).context("could not encode scene document")?;
        for operation in operations {
            apply_operation(&mut document, operation)?;
        }
        let candidate: SceneSpec = serde_json::from_value(document)
            .context("patched document does not match SceneSpec")?;
        let canonical_yaml = candidate
            .to_yaml()
            .context("could not serialize patched scene")?;
        parse_scene_spec(&canonical_yaml).context("patched scene is invalid")?;

        let revision = self.write_revision(
            project_id,
            Some(base_revision.to_owned()),
            &canonical_yaml,
            operations.to_vec(),
        )?;
        if promote {
            project.current_revision.clone_from(&revision.id);
            self.write_project(&project)?;
        }
        Ok(revision)
    }

    pub fn promote(
        &self,
        project_id: &str,
        revision_id: &str,
        expected_current_revision: Option<&str>,
    ) -> Result<Project> {
        self.scene(project_id, Some(revision_id))?;
        let mut project = self.project(project_id)?;
        if let Some(expected) = expected_current_revision
            && project.current_revision != expected
        {
            bail!(
                "revision conflict: project current revision is {}, not {expected}",
                project.current_revision
            );
        }
        project.current_revision = revision_id.to_owned();
        self.write_project(&project)?;
        Ok(project)
    }

    #[must_use]
    pub fn parameters() -> Vec<ParameterDescriptor> {
        let mut parameters = vec![
            parameter(
                "name",
                "string",
                None,
                None,
                ChangeCost::Metadata,
                "Human-readable scene and default output name",
            ),
            parameter(
                "seed",
                "integer",
                Some(0.0),
                Some(f64::from(u32::MAX)),
                ChangeCost::FullRender,
                "Deterministic sampling and automatic-search seed",
            ),
            parameter(
                "precision",
                "enum",
                None,
                None,
                ChangeCost::Pipeline,
                "Coordinate precision: f32 or quad-float",
            ),
            parameter(
                "camera.position",
                "vec3",
                None,
                None,
                ChangeCost::Uniform,
                "Camera position in world coordinates",
            ),
            parameter(
                "camera.target",
                "vec3",
                None,
                None,
                ChangeCost::Uniform,
                "World-space point kept at the center of view",
            ),
            parameter(
                "camera.up",
                "vec3",
                None,
                None,
                ChangeCost::Uniform,
                "Preferred camera vertical direction",
            ),
            parameter(
                "camera.vertical_fov_degrees",
                "float",
                Some(1.0),
                Some(179.0),
                ChangeCost::Uniform,
                "Vertical field of view",
            ),
            parameter(
                "camera.aperture_radius",
                "float",
                Some(0.0),
                None,
                ChangeCost::Uniform,
                "Thin-lens aperture radius",
            ),
            parameter(
                "camera.focus_distance",
                "float",
                Some(0.0),
                None,
                ChangeCost::Uniform,
                "Distance to the plane of sharp focus",
            ),
            parameter(
                "light.direction",
                "vec3",
                None,
                None,
                ChangeCost::Uniform,
                "Direction from a surface toward the directional light",
            ),
            parameter(
                "quality.post_process.exposure_stops",
                "float",
                Some(-20.0),
                Some(20.0),
                ChangeCost::PostProcess,
                "Artistic exposure adjustment",
            ),
            parameter(
                "quality.post_process.contrast",
                "float",
                Some(0.01),
                Some(8.0),
                ChangeCost::PostProcess,
                "Display-referred contrast",
            ),
            parameter(
                "quality.post_process.saturation",
                "float",
                Some(0.0),
                Some(8.0),
                ChangeCost::PostProcess,
                "Display-referred saturation",
            ),
            parameter(
                "animation.path.parameters.revolutions",
                "float",
                None,
                None,
                ChangeCost::Uniform,
                "Signed target-orbit turns over its duration",
            ),
            parameter(
                "animation.path.parameters.axis",
                "vec3",
                None,
                None,
                ChangeCost::Uniform,
                "Target-orbit axis",
            ),
            parameter(
                "animation.path.parameters.cone_angle_degrees",
                "float",
                Some(0.0),
                Some(180.0),
                ChangeCost::Uniform,
                "Target-orbit cone angle",
            ),
        ];
        parameters.extend([
            parameter(
                "fractal.kind",
                "enum",
                None,
                None,
                ChangeCost::Pipeline,
                "Fractal implementation or typed DSL",
            ),
            parameter(
                "fractal.parameters.iterations",
                "integer",
                Some(1.0),
                Some(128.0),
                ChangeCost::Pipeline,
                "Distance-estimator iteration count",
            ),
            parameter(
                "fractal.parameters.power",
                "float",
                Some(2.0),
                Some(32.0),
                ChangeCost::Uniform,
                "Mandelbulb power",
            ),
            parameter(
                "fractal.parameters.bailout",
                "float",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Escape bailout",
            ),
            parameter(
                "fractal.parameters.scale",
                "float",
                Some(-4.0),
                Some(4.0),
                ChangeCost::Uniform,
                "Mandelbox scale",
            ),
            parameter(
                "fractal.parameters.min_radius_squared",
                "float",
                Some(0.0),
                None,
                ChangeCost::Uniform,
                "Mandelbox minimum fold radius squared",
            ),
            parameter(
                "fractal.parameters.fixed_radius_squared",
                "float",
                Some(0.0),
                None,
                ChangeCost::Uniform,
                "Mandelbox fixed fold radius squared",
            ),
            parameter(
                "fractal.parameters.fold_limit",
                "float",
                Some(0.0),
                None,
                ChangeCost::Uniform,
                "Mandelbox box-fold limit",
            ),
            parameter(
                "fractal.parameters.orbit",
                "array",
                None,
                None,
                ChangeCost::Pipeline,
                "Bounded typed fractal transform program",
            ),
            parameter(
                "fractal.parameters.orbit_period",
                "integer",
                Some(1.0),
                Some(128.0),
                ChangeCost::Pipeline,
                "Hybrid orbit scheduling period",
            ),
            parameter(
                "fractal.parameters.color_iterations",
                "integer",
                Some(1.0),
                Some(512.0),
                ChangeCost::Pipeline,
                "Orbit-coloring iteration count",
            ),
            parameter(
                "fractal.parameters.normal_epsilon",
                "float",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "DSL normal gradient epsilon",
            ),
            parameter(
                "fractal.parameters.material.base_color",
                "rgb",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Surface base color",
            ),
            parameter(
                "fractal.parameters.material.accent_color",
                "rgb",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Surface accent color",
            ),
            parameter(
                "fractal.parameters.material.specular_color",
                "rgb",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Specular highlight color",
            ),
            parameter(
                "fractal.parameters.material.background_bottom",
                "rgb",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Lower background color",
            ),
            parameter(
                "fractal.parameters.material.background_top",
                "rgb",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Upper background color",
            ),
            parameter(
                "fractal.parameters.material.surface_palette",
                "array",
                None,
                None,
                ChangeCost::Pipeline,
                "Piecewise-linear surface palette",
            ),
            parameter(
                "fractal.parameters.material.orbit_palette_weight",
                "float",
                Some(0.0),
                Some(1.0),
                ChangeCost::Pipeline,
                "Orbit-coloring blend weight",
            ),
            parameter(
                "fractal.parameters.material.palette_offset",
                "float",
                None,
                None,
                ChangeCost::Pipeline,
                "Cyclic palette offset",
            ),
            parameter(
                "fractal.parameters.material.ambient_strength",
                "float",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Material ambient response",
            ),
            parameter(
                "fractal.parameters.material.diffuse_strength",
                "float",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Material diffuse response",
            ),
            parameter(
                "fractal.parameters.material.specular_strength",
                "float",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Material specular response",
            ),
            parameter(
                "fractal.parameters.material.shininess",
                "float",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Specular exponent",
            ),
            parameter(
                "fractal.parameters.material.metallic_specular_strength",
                "float",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Tinted metallic highlight strength",
            ),
            parameter(
                "fractal.parameters.material.metallic_shininess",
                "float",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Metallic highlight exponent",
            ),
            parameter(
                "fractal.parameters.material.rim_strength",
                "float",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Fresnel-like rim contribution",
            ),
            parameter(
                "fractal.parameters.material.fog_density",
                "float",
                Some(0.0),
                None,
                ChangeCost::Pipeline,
                "Distance fog density",
            ),
            parameter(
                "render.width",
                "integer",
                Some(1.0),
                Some(8192.0),
                ChangeCost::Pipeline,
                "Output viewport width",
            ),
            parameter(
                "render.height",
                "integer",
                Some(1.0),
                Some(8192.0),
                ChangeCost::Pipeline,
                "Output viewport height",
            ),
            parameter(
                "render.max_steps",
                "integer",
                Some(1.0),
                Some(1024.0),
                ChangeCost::Uniform,
                "Primary ray-march step budget",
            ),
            parameter(
                "render.max_distance",
                "float",
                Some(0.0),
                None,
                ChangeCost::Uniform,
                "Primary ray maximum distance",
            ),
            parameter(
                "render.epsilon",
                "float",
                Some(0.0),
                Some(0.1),
                ChangeCost::Uniform,
                "Base surface hit epsilon",
            ),
            parameter(
                "render.step_safety",
                "float",
                Some(0.0),
                Some(1.0),
                ChangeCost::Uniform,
                "DE step multiplier",
            ),
            parameter(
                "render.pixel_epsilon_multiplier",
                "float",
                Some(0.0),
                Some(10.0),
                ChangeCost::Uniform,
                "Projected pixel hit tolerance multiplier",
            ),
            parameter(
                "quality.samples_per_pixel",
                "integer",
                Some(1.0),
                Some(128.0),
                ChangeCost::Uniform,
                "Accumulated camera samples",
            ),
            parameter(
                "quality.ambient_occlusion",
                "object",
                None,
                None,
                ChangeCost::Uniform,
                "Ambient-occlusion controls",
            ),
            parameter(
                "quality.soft_shadow",
                "object",
                None,
                None,
                ChangeCost::Uniform,
                "Directional soft-shadow controls",
            ),
            parameter(
                "quality.reflection",
                "object",
                None,
                None,
                ChangeCost::Uniform,
                "One-bounce reflection controls",
            ),
            parameter(
                "quality.tone_mapping",
                "object",
                None,
                None,
                ChangeCost::PostProcess,
                "HDR tone-mapping controls",
            ),
            parameter(
                "quality.post_process",
                "object",
                None,
                None,
                ChangeCost::PostProcess,
                "Final display-referred grade",
            ),
            parameter(
                "animation.fps",
                "integer",
                Some(1.0),
                Some(240.0),
                ChangeCost::FullRender,
                "Animation frame rate",
            ),
            parameter(
                "animation.frame_count",
                "integer",
                Some(1.0),
                Some(1_000_000.0),
                ChangeCost::FullRender,
                "Animation frame count",
            ),
            parameter(
                "animation.path",
                "object",
                None,
                None,
                ChangeCost::FullRender,
                "Camera route and timing",
            ),
            parameter(
                "video.codec",
                "string",
                None,
                None,
                ChangeCost::Encode,
                "FFmpeg video codec",
            ),
            parameter(
                "video.pixel_format",
                "string",
                None,
                None,
                ChangeCost::Encode,
                "Encoded pixel format",
            ),
            parameter(
                "video.crf",
                "integer",
                Some(0.0),
                Some(63.0),
                ChangeCost::Encode,
                "Codec constant-quality value",
            ),
            parameter(
                "video.preset",
                "string",
                None,
                None,
                ChangeCost::Encode,
                "Codec speed/quality preset",
            ),
            parameter(
                "video.faststart",
                "boolean",
                None,
                None,
                ChangeCost::Encode,
                "Move stream metadata for progressive playback",
            ),
        ]);
        parameters.sort_by(|left, right| left.path.cmp(&right.path));
        parameters
    }

    pub(crate) fn run_directory(&self, project_id: &str, job_id: &str) -> Result<PathBuf> {
        validate_id("job", job_id)?;
        Ok(self
            .project_directory(project_id)?
            .join("runs")
            .join(job_id))
    }

    pub(crate) fn scene_path(&self, project_id: &str, revision_id: &str) -> Result<PathBuf> {
        validate_id("revision", revision_id)?;
        Ok(self
            .project_directory(project_id)?
            .join("revisions")
            .join(format!("{revision_id}.scene.yaml")))
    }

    fn write_revision(
        &self,
        project_id: &str,
        parent_revision: Option<String>,
        canonical_yaml: &str,
        changes: Vec<PatchOperation>,
    ) -> Result<Revision> {
        let scene_hash = sha256_hex(canonical_yaml.as_bytes());
        let id = format!("rev-{}", &scene_hash[..16]);
        let revision = Revision {
            id: id.clone(),
            scene_hash,
            parent_revision,
            created_unix_ms: unix_time_ms(),
            changes,
        };
        let directory = self.project_directory(project_id)?.join("revisions");
        fs::create_dir_all(&directory)?;
        let scene_path = directory.join(format!("{id}.scene.yaml"));
        let metadata_path = directory.join(format!("{id}.json"));
        if scene_path.exists() {
            let existing = fs::read_to_string(&scene_path)?;
            if sha256_hex(existing.as_bytes()) != revision.scene_hash {
                bail!("content-addressed revision collision for {id}");
            }
        } else {
            write_atomic(&scene_path, canonical_yaml.as_bytes())?;
        }
        if !metadata_path.exists() {
            write_json_atomic(&metadata_path, &revision)?;
        }
        Ok(revision)
    }

    fn write_project(&self, project: &Project) -> Result<()> {
        write_json_atomic(&self.project_file(&project.id)?, project)
    }

    fn project_file(&self, project_id: &str) -> Result<PathBuf> {
        Ok(self.project_directory(project_id)?.join("project.json"))
    }

    fn project_directory(&self, project_id: &str) -> Result<PathBuf> {
        validate_id("project", project_id)?;
        Ok(self.root.join("projects").join(project_id))
    }
}

pub(crate) fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).context("could not encode JSON")?;
    write_atomic(path, &bytes)
}

pub(crate) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn parameter(
    path: &str,
    value_type: &str,
    minimum: Option<f64>,
    maximum: Option<f64>,
    change_cost: ChangeCost,
    description: &str,
) -> ParameterDescriptor {
    ParameterDescriptor {
        path: path.to_owned(),
        value_type: value_type.to_owned(),
        minimum,
        maximum,
        change_cost,
        description: description.to_owned(),
    }
}

fn apply_operation(document: &mut serde_json::Value, operation: &PatchOperation) -> Result<()> {
    match operation {
        PatchOperation::Set { path, value } => {
            let tokens = path_tokens(path)?;
            let target = value_at_mut(document, &tokens, true)?;
            *target = value.clone();
        }
        PatchOperation::Remove { path } => {
            let tokens = path_tokens(path)?;
            let (parent_tokens, final_token) = tokens
                .split_last()
                .map(|(last, parent)| (parent, last))
                .ok_or_else(|| anyhow!("cannot remove the scene root"))?;
            let parent = value_at_mut(document, parent_tokens, false)?;
            match parent {
                serde_json::Value::Object(map) => {
                    if map.remove(final_token).is_none() {
                        bail!("scene path {path} does not exist");
                    }
                }
                serde_json::Value::Array(values) => {
                    let index = final_token.parse::<usize>().with_context(|| {
                        format!("scene path {path} does not select an array item")
                    })?;
                    if index >= values.len() {
                        bail!("scene array path {path} is out of bounds");
                    }
                    values.remove(index);
                }
                _ => bail!("scene path {path} has no removable child"),
            }
        }
        PatchOperation::Insert { path, index, value } => {
            let tokens = path_tokens(path)?;
            let target = value_at_mut(document, &tokens, false)?;
            let serde_json::Value::Array(values) = target else {
                bail!("scene path {path} is not an array");
            };
            if *index > values.len() {
                bail!("insert index {index} is outside scene array {path}");
            }
            values.insert(*index, value.clone());
        }
    }
    Ok(())
}

fn path_tokens(path: &str) -> Result<Vec<String>> {
    let tokens = if path.starts_with('/') {
        path.split('/')
            .skip(1)
            .map(|token| token.replace("~1", "/").replace("~0", "~"))
            .collect::<Vec<_>>()
    } else {
        path.split('.').map(str::to_owned).collect::<Vec<_>>()
    };
    if tokens.is_empty() || tokens.iter().any(String::is_empty) {
        bail!("scene patch path must not be empty");
    }
    Ok(tokens)
}

fn value_at_mut<'a>(
    mut value: &'a mut serde_json::Value,
    tokens: &[String],
    allow_final_object_insert: bool,
) -> Result<&'a mut serde_json::Value> {
    for (index, token) in tokens.iter().enumerate() {
        let final_token = index + 1 == tokens.len();
        match value {
            serde_json::Value::Object(map) => {
                if final_token && allow_final_object_insert && !map.contains_key(token) {
                    map.insert(token.clone(), serde_json::Value::Null);
                }
                value = map
                    .get_mut(token)
                    .ok_or_else(|| anyhow!("scene path component {token} does not exist"))?;
            }
            serde_json::Value::Array(values) => {
                let item = token
                    .parse::<usize>()
                    .with_context(|| format!("scene array component {token} is not an index"))?;
                value = values
                    .get_mut(item)
                    .ok_or_else(|| anyhow!("scene array index {item} is out of bounds"))?;
            }
            _ => bail!("scene path component {token} descends through a scalar"),
        }
    }
    Ok(value)
}

fn validate_id(kind: &str, id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 96
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{kind} id must contain only ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_STORE: AtomicU64 = AtomicU64::new(1);

    fn temporary_store() -> ProjectStore {
        let nonce = NEXT_TEST_STORE.fetch_add(1, Ordering::Relaxed);
        ProjectStore::new(std::env::temp_dir().join(format!(
            "fractal-workflow-project-{}-{nonce}",
            std::process::id()
        )))
    }

    #[test]
    fn patch_is_transactional_and_revision_checked() {
        let store = temporary_store();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scenes/examples/mandelbulb-target-orbit.yaml");
        let project = store.create("test", source).unwrap();
        let revision = store
            .patch(
                "test",
                &project.current_revision,
                &[PatchOperation::Set {
                    path: "animation.path.parameters.revolutions".to_owned(),
                    value: serde_json::json!(0.25),
                }],
                true,
            )
            .unwrap();
        assert_ne!(revision.id, project.current_revision);
        let (_, spec) = store.scene("test", Some(&revision.id)).unwrap();
        let value = serde_json::to_value(spec).unwrap();
        assert_eq!(
            value.pointer("/animation/path/parameters/revolutions"),
            Some(&serde_json::json!(0.25))
        );

        let before = store.project("test").unwrap();
        let failed = store.patch(
            "test",
            &revision.id,
            &[PatchOperation::Set {
                path: "camera.vertical_fov_degrees".to_owned(),
                value: serde_json::json!(500.0),
            }],
            true,
        );
        assert!(failed.is_err());
        assert_eq!(
            store.project("test").unwrap().current_revision,
            before.current_revision
        );
        fs::remove_dir_all(store.root()).unwrap();
    }

    #[test]
    fn stale_revision_cannot_overwrite_current_project() {
        let store = temporary_store();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scenes/examples/mandelbulb-target-orbit.yaml");
        let project = store.create("test", source).unwrap();
        store
            .patch(
                "test",
                &project.current_revision,
                &[PatchOperation::Set {
                    path: "seed".to_owned(),
                    value: serde_json::json!(1234),
                }],
                true,
            )
            .unwrap();
        let error = store
            .patch(
                "test",
                &project.current_revision,
                &[PatchOperation::Set {
                    path: "seed".to_owned(),
                    value: serde_json::json!(5678),
                }],
                true,
            )
            .unwrap_err();
        assert!(error.to_string().contains("revision conflict"));
        fs::remove_dir_all(store.root()).unwrap();
    }

    #[test]
    fn candidate_revisions_branch_without_moving_current() {
        let store = temporary_store();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../scenes/examples/mandelbulb-target-orbit.yaml");
        let project = store.create("test", source).unwrap();
        let first = store
            .patch(
                "test",
                &project.current_revision,
                &[PatchOperation::Set {
                    path: "animation.path.parameters.revolutions".to_owned(),
                    value: serde_json::json!(0.25),
                }],
                false,
            )
            .unwrap();
        let second = store
            .patch(
                "test",
                &project.current_revision,
                &[PatchOperation::Set {
                    path: "animation.path.parameters.revolutions".to_owned(),
                    value: serde_json::json!(-0.25),
                }],
                false,
            )
            .unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(
            store.project("test").unwrap().current_revision,
            project.current_revision
        );
        store
            .promote("test", &second.id, Some(&project.current_revision))
            .unwrap();
        assert_eq!(store.project("test").unwrap().current_revision, second.id);
        fs::remove_dir_all(store.root()).unwrap();
    }
}
