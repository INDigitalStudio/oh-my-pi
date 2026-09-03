//! Harness-owned dynamic devices for media generation and `AutoQA` reports.

use std::{
	collections::BTreeMap,
	fs::{self, OpenOptions},
	io::{self, Read as _},
	path::{Component, Path, PathBuf},
	sync::Arc,
	time::{SystemTime, UNIX_EPOCH},
};

use async_stream::stream;
use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use omp_cache::telemetry_cache::{StoredIssue, TelemetryIndex};
use omp_core::{ArtifactUrl, Str, sf};
use omp_proto::{
	inference::v1::{self as inference_pb, generate_image_request, value},
	thread::v1::{self as thread_pb, blob},
};
use omp_tool::{
	Abort, ArgIssue, ArgIssueKind, CommitError, Constraint, DocEffects, Effects, Ev, IncomingParams,
	InferenceEffects, LiftedCall, ParamError, Part, PromptCaps, RecordedCall, Rev, Tool, ToolSpec,
	ToolTerminal,
};
use omp_tools::{ask, read::image::sniff_metadata};
use omp_voice::audio::PlaybackStream;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
	blobs::{BlobError, BlobHost},
	search_backend::SearchBridgeHost,
};

/// Production ask-dialog vocalizer over the application inference facade.
pub struct AskVocalizer {
	backend: Arc<SearchBridgeHost>,
}

#[async_trait]
/// Synthesizes and plays one complete dialog through local Kokoro by default.
#[async_trait]
impl omp_tools::ask::AskVocalizer for AskVocalizer {
	async fn speak(
		&self,
		lines: &[omp_tools::ask::SpokenLine],
		cancellation: CancellationToken,
	) -> Result<(), ask::Fault> {
		if cancellation.is_cancelled() || lines.is_empty() {
			return Ok(());
		}
		let mut text = String::new();
		for line in lines {
			if !text.is_empty() {
				text.push_str(". ");
			}
			text.push_str(line.text.as_str());
		}
		let request = inference_pb::SpeakRequest {
			model: "kokoro".to_owned(),
			text,
			voice: "af_heart".to_owned(),
			encoding: inference_pb::AudioEncoding::Pcm16 as i32,
			sample_rate_hz: Some(24_000),
			speed: None,
			instructions: String::new(),
			clone: None,
			props: None,
		};
		let audio = tokio::select! {
			biased;
			() = cancellation.cancelled() => {
				return Err(omp_tools::ask::Fault::Presenter {
					message: sf!("ask speech was cancelled"),
				});
			},
			audio = self.backend.speak(request) => audio.map_err(|error| {
				omp_tools::ask::Fault::Presenter { message: error.message }
			})?,
		};
		if audio.len() % 2 != 0 {
			return Err(ask::Fault::Presenter {
				message: sf!("ask speech returned malformed PCM16 audio"),
			});
		}
		let samples = audio
			.chunks_exact(2)
			.map(|sample| f32::from(i16::from_le_bytes([sample[0], sample[1]])) / f32::from(i16::MAX))
			.collect::<Vec<_>>();
		let mut playback = PlaybackStream::start(24_000)
			.map_err(|error| ask::Fault::Presenter { message: Str::from(error.to_string()) })?;
		playback
			.writer()
			.and_then(|writer| writer.write_owned(samples))
			.map_err(|error| ask::Fault::Presenter { message: Str::from(error.to_string()) })?;
		tokio::select! {
			biased;
			() = cancellation.cancelled() => {
				let _ = playback.abort();
				Err(omp_tools::ask::Fault::Presenter {
					message: sf!("ask speech was cancelled"),
				})
			},
			result = playback.drain() => result.map_err(|error| {
				omp_tools::ask::Fault::Presenter { message: Str::from(error.to_string()) }
			}),
		}
	}
}

/// Creates the production ask-dialog vocalizer.
pub(crate) fn ask_vocalizer(
	backend: Arc<SearchBridgeHost>,
) -> Arc<dyn omp_tools::ask::AskVocalizer> {
	Arc::new(AskVocalizer { backend })
}
/// Largest `input_image` accepted for image-to-image generation (35 MiB).
const MAX_INPUT_IMAGE_BYTES: u64 = 35 * 1024 * 1024;
/// Longest `text` accepted by `tts`, in characters.
const MAX_SPEECH_CHARS: usize = 15_000;
const MAX_INPUT_IMAGE_BASE64_BYTES: usize = ((MAX_INPUT_IMAGE_BYTES as usize + 2) / 3) * 4;

/// One image-to-image input, provided either by contained path or inline data.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageInput {
	/// Workspace-relative input image path.
	pub path:      Option<Str>,
	/// Base64-encoded input image bytes.
	pub data:      Option<Str>,
	/// Optional MIME assertion checked against the sniffed bytes.
	pub mime_type: Option<Str>,
}

/// Media generation arguments. The published per-device schema exposes only
/// the fields belonging to that device.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MediaParams {
	/// Main image subject; required by `image_gen`.
	pub subject:      Option<Str>,
	/// What the image subject is doing.
	pub action:       Option<Str>,
	/// Image location or environment.
	pub scene:        Option<Str>,
	/// Camera angle and framing.
	pub composition:  Option<Str>,
	/// Lighting setup.
	pub lighting:     Option<Str>,
	/// Artistic style.
	pub style:        Option<Str>,
	/// Text to render in the image.
	pub text:         Option<Str>,
	/// Edits to apply to supplied images.
	pub changes:      Option<Vec<Str>>,
	/// Image aspect ratio such as `1:1` or `16:9`.
	pub aspect_ratio: Option<Str>,
	/// Requested image dimensions.
	pub image_size:   Option<Str>,
	/// Input images supplied by contained path or base64.
	pub input:        Option<Vec<ImageInput>>,
	/// Requested image provider.
	pub provider:     Option<Str>,
	/// Explicit provider voice ID.
	pub voice_id:     Option<Str>,
	/// BCP-47 speech language hint.
	pub language:     Option<Str>,
	/// Requested speech sample rate in hertz.
	pub sample_rate:  Option<u32>,
	/// Requested speech bit rate in bits per second.
	pub bit_rate:     Option<u32>,
	/// Workspace-relative file receiving generated bytes atomically. Required
	/// by `tts` and optional for `image_gen`.
	pub output_path:  Option<Str>,
}

/// A generated media artifact. Unavailable backends never fabricate one.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MediaPayload {
	/// Content-addressed artifact id.
	pub artifact_id: Str,
	/// Produced MIME type.
	pub media_type:  Str,
	/// Workspace-relative output path when file output was requested.
	pub output_path: Option<Str>,
	/// Durable bytes exposed to dynamic callers without erasing media typing.
	#[serde(default)]
	pub blob:        Option<omp_tool::BlobRef>,
}

/// Stable structured media backend failure.
#[derive(Clone, Debug, Deserialize, Eq, Error, PartialEq, Serialize)]
#[error("{code}: {message}")]
pub struct MediaFault {
	/// Machine-readable failure category.
	pub code:    Str,
	/// Backend that could not serve the request.
	pub backend: Str,
	/// Human-readable explanation.
	pub message: Str,
}

/// Media devices do not currently stream previews.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MediaUpdate {}

#[derive(Clone, Copy)]
enum MediaKind {
	Image,
	Speech,
}

/// Dyn-mounted media generator.
pub struct MediaDevice {
	spec:    ToolSpec,
	kind:    MediaKind,
	backend: Arc<SearchBridgeHost>,
	blobs:   BlobHost,
	root:    PathBuf,
}

/// Creates the `image_gen@3` dynamic device.
pub fn image_gen(backend: Arc<SearchBridgeHost>, blobs: BlobHost, root: PathBuf) -> MediaDevice {
	media_device(
		"image_gen",
		"Generates images from a structured subject/action/scene/composition/lighting/style prompt. \
		 `input` accepts contained workspace paths or base64 PNG/JPEG/GIF/WebP images up to 35 MiB; \
		 `output_path` additionally writes the result atomically inside the workspace.",
		MediaKind::Image,
		backend,
		blobs,
		root,
	)
}

/// Creates the `tts@3` dynamic device.
pub fn tts(backend: Arc<SearchBridgeHost>, blobs: BlobHost, root: PathBuf) -> MediaDevice {
	media_device(
		"tts",
		"Synthesizes speech with local Kokoro or a configured remote voice backend. `text` is \
		 1–15000 characters and `output_path` is required: the audio is written there atomically \
		 inside the workspace and also retained as an artifact.",
		MediaKind::Speech,
		backend,
		blobs,
		root,
	)
}

fn media_device(
	name: &'static str,
	description: &'static str,
	kind: MediaKind,
	backend: Arc<SearchBridgeHost>,
	blobs: BlobHost,
	root: PathBuf,
) -> MediaDevice {
	MediaDevice {
		spec: ToolSpec {
			name:            sf!(name),
			rev:             Rev { family: Default::default(), n: 3 },
			description:     sf!(description),
			schema:          media_schema(kind),
			constraint:      Constraint::Schema {
				priority:       100,
				on_unsupported: omp_tool::Fallback::Unspecified,
			},
			effects:         Effects {
				documents: Some(DocEffects { read: true, write_globs: Arc::from([sf!("**")]) }),
				exec:      None,
				inference: Some(InferenceEffects { max_requests: 1, max_usd: Default::default() }),
				desktop:   None,
				subagents: 0,
			},
			projection_code: omp_tool::native_projection_code(
				env!("CARGO_PKG_NAME"),
				env!("CARGO_PKG_VERSION"),
				include_bytes!("media_devices.rs"),
			)
			.into_bytes(),
		},
		kind,
		backend,
		blobs,
		root,
	}
}

fn media_schema(kind: MediaKind) -> Bytes {
	let schema = omp_tool::schema::<MediaParams>();
	let mut value: Value =
		serde_json::from_slice(&schema).expect("generated media schema must be valid JSON");
	let object = value
		.as_object_mut()
		.expect("media parameters use an object schema");
	let properties = object
		.get_mut("properties")
		.and_then(Value::as_object_mut)
		.expect("generated media schema has properties");
	let allowed: &[&str] = match kind {
		MediaKind::Image => &[
			"i",
			"notrunc",
			"subject",
			"action",
			"scene",
			"composition",
			"lighting",
			"style",
			"text",
			"changes",
			"aspect_ratio",
			"image_size",
			"input",
			"provider",
			"output_path",
		],
		MediaKind::Speech => {
			&["i", "notrunc", "text", "voice_id", "language", "output_path", "sample_rate", "bit_rate"]
		},
	};
	properties.retain(|name, _| allowed.contains(&name.as_str()));
	object.insert("required".to_owned(), match kind {
		MediaKind::Image => serde_json::json!(["i", "subject"]),
		MediaKind::Speech => serde_json::json!(["i", "text", "output_path"]),
	});
	Bytes::from(serde_json::to_vec(&value).expect("media schema serialization is infallible"))
}

impl MediaDevice {
	async fn generate_image(&self, params: &MediaParams) -> Result<MediaPayload, MediaFault> {
		let output = params
			.output_path
			.as_deref()
			.map(|path| OutputTarget::resolve(&self.root, path))
			.transpose()?;
		let request = image_request(&self.root, params)?;
		let mut images = self
			.backend
			.generate_image(request)
			.await
			.map_err(media_backend_fault)?;
		let image = images.drain(..).next().ok_or_else(|| {
			media_fault("image_empty", "inference", "image generation returned no artifact")
		})?;
		finish_image(&self.blobs, output.as_ref(), &image)
	}

	async fn synthesize_speech(&self, params: &MediaParams) -> Result<MediaPayload, MediaFault> {
		let output = OutputTarget::resolve(
			&self.root,
			params
				.output_path
				.as_deref()
				.expect("validated speech output path"),
		)?;
		let format = speech_format(
			params
				.output_path
				.as_deref()
				.expect("validated speech output path"),
		)?;
		let request = inference_pb::SpeakRequest {
			model:          "kokoro".to_owned(),
			text:           params
				.text
				.as_deref()
				.expect("validated speech text")
				.to_owned(),
			voice:          params.voice_id.as_deref().unwrap_or("af_heart").to_owned(),
			encoding:       audio_encoding(format)?,
			sample_rate_hz: params.sample_rate,
			speed:          None,
			instructions:   params
				.language
				.as_deref()
				.map_or_else(String::new, |language| format!("Speak in {language}.")),
			clone:          None,
			props:          params.bit_rate.map(|bit_rate| inference_pb::ValueMap {
				fields: BTreeMap::from([("audio/bit_rate".to_owned(), inference_pb::Value {
					kind: Some(value::Kind::Uint(u64::from(bit_rate))),
				})]),
			}),
		};
		let audio = self
			.backend
			.speak(request)
			.await
			.map_err(media_backend_fault)?;
		let id = self.blobs.put(&audio).map_err(media_blob_fault)?;
		let output_path = Some(output.write(&audio)?);
		let media_type = Str::new(match format {
			"mp3" => "audio/mpeg",
			"wav" => "audio/wav",
			"pcm16" => "audio/L16",
			"opus" => "audio/opus",
			"aac" => "audio/aac",
			"flac" => "audio/flac",
			_ => unreachable!("validated audio format"),
		});
		Ok(MediaPayload {
			artifact_id: Str::new(ArtifactUrl::from_digest(id.hash).as_str()),
			media_type: media_type.clone(),
			output_path,
			blob: Some(omp_tool::BlobRef {
				hash: Str::new(omp_core::Hash32::new(id.hash).to_hex().as_str()),
				media_type,
				byte_len: id.size,
			}),
		})
	}
}
/// Syntactic pre-commit checks for `image_gen`.
fn validate_image_params(params: &MediaParams) -> Result<(), MediaFault> {
	if params
		.subject
		.as_deref()
		.is_none_or(|subject| subject.trim().is_empty())
	{
		return Err(media_fault("invalid_media_request", "none", "subject must not be empty"));
	}
	if params.input.as_ref().is_some_and(|inputs| {
		inputs
			.iter()
			.any(|input| input.path.is_some() == input.data.is_some())
	}) {
		return Err(media_fault(
			"invalid_input_image",
			"filesystem",
			"each input image must provide exactly one of path or data",
		));
	}
	Ok(())
}

/// Syntactic pre-commit checks for `tts`: nonempty text of at most 15,000
/// characters and a required `output_path`.
fn validate_speech_params(params: &MediaParams) -> Result<(), MediaFault> {
	let text = params.text.as_deref().unwrap_or("");
	if text.trim().is_empty() {
		return Err(media_fault("invalid_media_request", "none", "text must not be empty"));
	}
	if text.chars().count() > MAX_SPEECH_CHARS {
		return Err(media_fault(
			"text_too_long",
			"none",
			"text must be at most 15000 characters; split longer passages into several calls",
		));
	}
	if params
		.output_path
		.as_deref()
		.is_none_or(|path| path.trim().is_empty())
	{
		return Err(media_fault(
			"output_path_required",
			"none",
			"output_path is required: tts writes the audio there atomically and also retains it as \
			 an artifact",
		));
	}
	Ok(())
}

/// Builds the inference request for `image_gen`, loading the validated
/// `input_image` from the workspace.
fn image_request(
	root: &Path,
	params: &MediaParams,
) -> Result<inference_pb::GenerateImageRequest, MediaFault> {
	let prompt = assemble_image_prompt(params);
	let input_images = params
		.input
		.as_deref()
		.unwrap_or_default()
		.iter()
		.map(|input| load_image_input(root, input))
		.collect::<Result<Vec<_>, _>>()?;
	Ok(inference_pb::GenerateImageRequest {
		model: params.provider.as_deref().unwrap_or_default().to_owned(),
		prompt,
		n: 1,
		aspect_ratio: aspect_ratio(params.aspect_ratio.as_deref())?,
		size: image_size(params.image_size.as_deref())?,
		quality: generate_image_request::Quality::Medium as i32,
		format: image_format(),
		background: generate_image_request::Background::Unspecified as i32,
		compression: None,
		seed: None,
		input_images,
		props: None,
	})
}

fn image_size(
	value: Option<&str>,
) -> Result<Option<generate_image_request::ImageSize>, MediaFault> {
	Ok(match value {
		None => None,
		Some("1024x1024") => Some(generate_image_request::ImageSize { width: 1024, height: 1024 }),
		Some("1536x1024") => Some(generate_image_request::ImageSize { width: 1536, height: 1024 }),
		Some("1024x1536") => Some(generate_image_request::ImageSize { width: 1024, height: 1536 }),
		Some(_) => {
			return Err(media_fault("invalid_image_size", "image", "unsupported image size"));
		},
	})
}

fn assemble_image_prompt(params: &MediaParams) -> String {
	let mut sentences = Vec::new();
	let mut subject = params
		.subject
		.as_deref()
		.expect("validated image subject")
		.to_owned();
	for detail in [params.action.as_deref(), params.scene.as_deref()]
		.into_iter()
		.flatten()
		.filter(|detail| !detail.trim().is_empty())
	{
		subject.push_str(", ");
		subject.push_str(detail);
	}
	sentences.push(subject);
	for detail in
		[params.composition.as_deref(), params.lighting.as_deref(), params.style.as_deref()]
	{
		if let Some(detail) = detail.filter(|detail| !detail.trim().is_empty()) {
			sentences.push(detail.to_owned());
		}
	}
	if let Some(text) = params
		.text
		.as_deref()
		.filter(|text| !text.trim().is_empty())
	{
		sentences.push(format!("Render this text exactly: {text}"));
	}
	if let Some(changes) = params
		.changes
		.as_deref()
		.filter(|changes| !changes.is_empty())
	{
		sentences.push(format!(
			"Apply these changes: {}. Preserve all unspecified details.",
			changes.join("; ")
		));
	}
	sentences.join(". ")
}

fn load_image_input(root: &Path, input: &ImageInput) -> Result<thread_pb::Blob, MediaFault> {
	let image = if let Some(path) = input.path.as_deref() {
		load_input_image(root, path)?
	} else {
		let data = input.data.as_deref().expect("validated inline image");
		if data.len() > MAX_INPUT_IMAGE_BASE64_BYTES {
			return Err(media_fault(
				"input_image_too_large",
				"input",
				"input image exceeds the 35 MiB limit",
			));
		}
		let bytes = omp_core::base64::decode(data.as_bytes())
			.into_vec()
			.map_err(|_| {
				media_fault("invalid_input_image", "input", "input image data is not valid base64")
			})?;
		image_blob(bytes)?
	};
	if input
		.mime_type
		.as_deref()
		.is_some_and(|asserted| asserted != image.mime)
	{
		return Err(media_fault(
			"input_image_mime_mismatch",
			"input",
			"input image MIME assertion does not match its bytes",
		));
	}
	Ok(image)
}

/// Retains a generated image as an artifact and, when requested, writes it to
/// the resolved output target.
fn finish_image(
	blobs: &BlobHost,
	output: Option<&OutputTarget>,
	image: &thread_pb::Blob,
) -> Result<MediaPayload, MediaFault> {
	let (artifact_id, blob) = store_blob(blobs, image)?;
	let output_path = output
		.map(|target| {
			if image.inline.is_empty() {
				return Err(media_fault(
					"image_bytes_unavailable",
					"inference",
					"provider returned a reference-only image; output_path cannot be written",
				));
			}
			target.write(&image.inline)
		})
		.transpose()?;
	Ok(MediaPayload {
		artifact_id,
		media_type: Str::new(if image.mime.is_empty() {
			"image/png"
		} else {
			&image.mime
		}),
		output_path,
		blob,
	})
}

/// Loads a workspace-contained `input_image`, enforcing the 35 MiB ceiling and
/// a recognized image signature.
fn load_input_image(root: &Path, authored: &str) -> Result<thread_pb::Blob, MediaFault> {
	let Some(relative) = workspace_relative(authored) else {
		return Err(media_fault(
			"invalid_input_image",
			"filesystem",
			"input_image must be workspace-relative and contained",
		));
	};
	let root = root.canonicalize().map_err(media_io_fault)?;
	let target = root.join(relative).canonicalize().map_err(|error| {
		if error.kind() == io::ErrorKind::NotFound {
			media_fault("input_image_missing", "filesystem", "input_image file not found")
		} else {
			media_io_fault(error)
		}
	})?;
	if !target.starts_with(&root) {
		return Err(media_fault(
			"invalid_input_image",
			"filesystem",
			"input_image escapes the workspace",
		));
	}
	let file = std::fs::File::open(&target).map_err(media_io_fault)?;
	let metadata = file.metadata().map_err(media_io_fault)?;
	if !metadata.is_file() {
		return Err(media_fault(
			"invalid_input_image",
			"filesystem",
			"input_image must be a regular file",
		));
	}
	if metadata.len() > MAX_INPUT_IMAGE_BYTES {
		return Err(media_fault(
			"input_image_too_large",
			"filesystem",
			"input_image exceeds the 35 MiB limit",
		));
	}
	let mut bytes =
		Vec::with_capacity(usize::try_from(metadata.len().min(MAX_INPUT_IMAGE_BYTES)).unwrap_or(0));
	file
		.take(MAX_INPUT_IMAGE_BYTES + 1)
		.read_to_end(&mut bytes)
		.map_err(media_io_fault)?;
	if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_INPUT_IMAGE_BYTES {
		return Err(media_fault(
			"input_image_too_large",
			"filesystem",
			"input_image exceeds the 35 MiB limit",
		));
	}
	image_blob(bytes)
}

fn image_blob(bytes: Vec<u8>) -> Result<thread_pb::Blob, MediaFault> {
	if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_INPUT_IMAGE_BYTES {
		return Err(media_fault(
			"input_image_too_large",
			"input",
			"input image exceeds the 35 MiB limit",
		));
	}
	let kind = sniff_metadata(&bytes)
		.map(|metadata| metadata.kind)
		.ok_or_else(|| {
			media_fault(
				"input_image_unsupported",
				"filesystem",
				"input_image is not a PNG, JPEG, GIF, or WebP image",
			)
		})?;
	Ok(thread_pb::Blob {
		hash:   bytes::Bytes::copy_from_slice(omp_core::Hash32::sum(&bytes).as_bytes()),
		mime:   kind.media_type().to_owned(),
		size:   u64::try_from(bytes.len()).unwrap_or(u64::MAX),
		inline: bytes.into(),
		detail: blob::Detail::Original as i32,
	})
}

/// Returns the authored path when it is a nonempty relative path without
/// parent, root, or prefix components.
fn workspace_relative(authored: &str) -> Option<&Path> {
	let path = Path::new(authored);
	let contained = !path.as_os_str().is_empty()
		&& !path.components().any(|component| {
			matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))
		});
	contained.then_some(path)
}

/// Workspace-contained `output_path` resolved before any provider call so an
/// unwritable destination never costs a generation.
#[derive(Debug)]
struct OutputTarget {
	authored:  Str,
	target:    PathBuf,
	temporary: PathBuf,
}

impl OutputTarget {
	fn resolve(root: &Path, authored: &str) -> Result<Self, MediaFault> {
		let Some(path) = workspace_relative(authored) else {
			return Err(media_fault(
				"invalid_output_path",
				"filesystem",
				"output_path must be workspace-relative and contained",
			));
		};
		let root = root.canonicalize().map_err(media_io_fault)?;
		let target = root.join(path);
		let parent = target.parent().ok_or_else(|| {
			media_fault("invalid_output_path", "filesystem", "output_path has no parent")
		})?;
		let canonical_parent = parent.canonicalize().map_err(media_io_fault)?;
		if !canonical_parent.starts_with(&root) {
			return Err(media_fault(
				"invalid_output_path",
				"filesystem",
				"output_path escapes the workspace",
			));
		}
		let file_name = target.file_name().ok_or_else(|| {
			media_fault("invalid_output_path", "filesystem", "output_path has no file name")
		})?;
		let temporary = canonical_parent.join(format!(
			".{}.omp-media-{}-{}",
			file_name.to_string_lossy(),
			std::process::id(),
			now_ms()
		));
		Ok(Self { authored: Str::new(authored), target: canonical_parent.join(file_name), temporary })
	}

	/// Writes `bytes` through a same-directory temporary file and an atomic
	/// rename, returning the authored path.
	fn write(&self, bytes: &[u8]) -> Result<Str, MediaFault> {
		let write_result = (|| -> io::Result<()> {
			use std::io::Write as _;
			let mut file = OpenOptions::new()
				.write(true)
				.create_new(true)
				.open(&self.temporary)?;
			file.write_all(bytes)?;
			file.sync_all()?;
			fs::rename(&self.temporary, &self.target)
		})();
		if let Err(error) = write_result {
			let _ = fs::remove_file(&self.temporary);
			return Err(media_io_fault(error));
		}
		Ok(self.authored.clone())
	}
}

impl Tool for MediaDevice {
	type Fault = MediaFault;
	type Params = MediaParams;
	type Payload = MediaPayload;
	type Update = MediaUpdate;

	fn spec(&self) -> &ToolSpec {
		&self.spec
	}

	fn call<'c>(
		&'c self,
		mut incoming: IncomingParams<'c>,
	) -> impl Stream<Item = Ev<MediaUpdate, MediaPayload, MediaFault>> + Send + 'c {
		stream! {
			let params = match incoming.whole::<MediaParams>().await {
				Ok(params) => params,
				Err(error) => { yield media_param_event(error); return; },
			};
			let validation = match self.kind {
				MediaKind::Image => validate_image_params(&params),
				MediaKind::Speech => validate_speech_params(&params),
			};
			if let Err(fault) = validation {
				yield media_done(Err(fault));
				return;
			}
			if let Err(error) = incoming.interruptable().committed().await {
				yield media_commit_event(error);
				return;
			}
			let result = match self.kind {
				MediaKind::Image => self.generate_image(&params).await,
				MediaKind::Speech => self.synthesize_speech(&params).await,
			};
			yield media_done(result);
		}
	}

	fn prompt(&self, view: Result<&MediaPayload, &MediaFault>, _: &PromptCaps) -> Vec<Part> {
		match view {
			Ok(payload) => payload.blob.clone().map_or_else(
				|| {
					vec![Part::Text {
						text: Str::from(format!(
							"Generated {} artifact {}",
							payload.media_type, payload.artifact_id
						)),
					}]
				},
				|blob| vec![Part::Blob { blob, alt: None }],
			),
			Err(fault) => vec![Part::Json {
				json: Bytes::from(
					serde_json::to_vec(fault).expect("media fault serialization is infallible"),
				),
			}],
		}
	}

	fn lift(&self, from: &Rev, call: RecordedCall<'_>) -> Option<LiftedCall> {
		if !from.family.is_empty() || !matches!(from.n, 1 | 2) {
			return None;
		}
		let mut arguments: Value = serde_json::from_slice(call.raw_args).ok()?;
		let object = arguments.as_object_mut()?;
		match self.kind {
			MediaKind::Image => {
				if let Some(prompt) = object.remove("prompt") {
					let subject = match prompt {
						Value::String(subject) => Value::String(subject),
						structured => Value::String(structured.to_string()),
					};
					object.insert("subject".to_owned(), subject);
				}
				if let Some(input) = object.remove("input_image") {
					object.insert(
						"input".to_owned(),
						Value::Array(vec![serde_json::json!({ "path": input })]),
					);
				}
				object.remove("format");
			},
			MediaKind::Speech => {
				object.remove("provider");
				object.remove("voice");
				object.remove("format");
			},
		}
		Some(LiftedCall {
			raw_args: Bytes::from(serde_json::to_vec(&arguments).ok()?),
			verdict:  Bytes::copy_from_slice(call.verdict),
		})
	}
}

/// Translates provider-neutral media vocabulary into the canonical inference
/// wire.
fn aspect_ratio(value: Option<&str>) -> Result<i32, MediaFault> {
	Ok(match value.unwrap_or("1:1") {
		"1:1" => 1,
		"16:9" => 2,
		"9:16" => 3,
		"4:3" => 4,
		"3:4" => 5,
		"3:2" => 6,
		"2:3" => 7,
		_ => return Err(media_fault("invalid_aspect_ratio", "image", "unsupported aspect ratio")),
	})
}

const fn image_format() -> i32 {
	generate_image_request::Format::Png as i32
}

fn speech_format(output_path: &str) -> Result<&'static str, MediaFault> {
	match Path::new(output_path)
		.extension()
		.and_then(|extension| extension.to_str())
		.map(str::to_ascii_lowercase)
		.as_deref()
	{
		Some("mp3") => Ok("mp3"),
		Some("wav") => Ok("wav"),
		_ => {
			Err(media_fault("invalid_audio_format", "speech", "output_path must end in .mp3 or .wav"))
		},
	}
}

fn audio_encoding(value: &str) -> Result<i32, MediaFault> {
	Ok(match value {
		"mp3" => inference_pb::AudioEncoding::Mp3 as i32,
		"pcm16" => inference_pb::AudioEncoding::Pcm16 as i32,
		"wav" => inference_pb::AudioEncoding::Wav as i32,
		"opus" => inference_pb::AudioEncoding::Opus as i32,
		"aac" => inference_pb::AudioEncoding::Aac as i32,
		"flac" => inference_pb::AudioEncoding::Flac as i32,
		_ => return Err(media_fault("invalid_audio_format", "speech", "unsupported audio format")),
	})
}

fn store_blob(
	blobs: &BlobHost,
	blob: &thread_pb::Blob,
) -> Result<(Str, Option<omp_tool::BlobRef>), MediaFault> {
	if blob.inline.is_empty() {
		let hash = <[u8; 32]>::try_from(blob.hash.as_ref()).map_err(|_| {
			media_fault("invalid_image_artifact", "inference", "image artifact has no bytes or digest")
		})?;
		return Ok((Str::new(ArtifactUrl::from_digest(hash).as_str()), None));
	}
	let id = blobs.put(&blob.inline).map_err(media_blob_fault)?;
	let media_type = Str::new(if blob.mime.is_empty() {
		"image/png"
	} else {
		blob.mime.as_str()
	});
	Ok((
		Str::new(ArtifactUrl::from_digest(id.hash).as_str()),
		Some(omp_tool::BlobRef {
			hash: Str::new(omp_core::Hash32::new(id.hash).to_hex().as_str()),
			media_type,
			byte_len: id.size,
		}),
	))
}

fn media_fault(code: &'static str, backend: &'static str, message: &'static str) -> MediaFault {
	MediaFault {
		code:    Str::new_static(code),
		backend: Str::new_static(backend),
		message: Str::new_static(message),
	}
}

fn media_backend_fault(error: omp_tools::web_search::BackendError) -> MediaFault {
	MediaFault { code: error.code, backend: sf!("inference"), message: error.message }
}

fn media_blob_fault(error: BlobError) -> MediaFault {
	MediaFault {
		code:    sf!("media_artifact_failed"),
		backend: sf!("blob"),
		message: Str::new(error.to_string()),
	}
}

fn media_io_fault(error: io::Error) -> MediaFault {
	MediaFault {
		code:    sf!("media_input_failed"),
		backend: sf!("filesystem"),
		message: Str::new(error.to_string()),
	}
}

/// Arguments accepted by `report_issue@1`.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReportParams {
	/// Session filing the report.
	pub session_id: Str,
	/// Device whose structured result was inconsistent.
	pub device:     Str,
	/// Observed device revision, required for exact `name@rev` attribution.
	pub rev:        Str,
	/// Structured verdict over the device result.
	pub verdict:    Value,
}

/// Durable `AutoQA` filing result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReportPayload {
	/// Stable issue identifier.
	pub issue_id: Str,
	/// Exact reported `name@rev` identity.
	pub target:   Str,
	/// Exact target revision fenced by a later consent decision.
	pub revision: Str,
	/// Redacted summary suitable for an explicit consent surface.
	pub summary:  Str,
	/// Firehose event kind written beside the issue.
	pub kind:     Str,
}

/// Dyn-mounted `AutoQA` issue recorder.
pub struct ReportIssue {
	spec:  ToolSpec,
	store: Arc<TelemetryIndex>,
}

/// Creates `report_issue@1` over the project `AutoQA` store.
pub fn report_issue(store: Arc<TelemetryIndex>) -> ReportIssue {
	ReportIssue {
		spec: ToolSpec {
			name:            sf!("report_issue"),
			rev:             Rev { family: Default::default(), n: 1 },
			description:     sf!(
				"Records a structured AutoQA verdict against an exact device revision in the durable \
				 local issue store.",
			),
			schema:          omp_tool::schema::<ReportParams>(),
			constraint:      Constraint::Schema {
				priority:       255,
				on_unsupported: omp_tool::Fallback::Unspecified,
			},
			effects:         Effects::empty(),
			projection_code: omp_tool::native_projection_code(
				env!("CARGO_PKG_NAME"),
				env!("CARGO_PKG_VERSION"),
				include_bytes!("media_devices.rs"),
			)
			.into_bytes(),
		},
		store,
	}
}

impl Tool for ReportIssue {
	type Fault = MediaFault;
	type Params = ReportParams;
	type Payload = ReportPayload;
	type Update = MediaUpdate;

	fn spec(&self) -> &ToolSpec {
		&self.spec
	}

	fn call<'c>(
		&'c self,
		mut incoming: IncomingParams<'c>,
	) -> impl Stream<Item = Ev<MediaUpdate, ReportPayload, MediaFault>> + Send + 'c {
		stream! {
			let params = match incoming.whole::<ReportParams>().await {
				Ok(params) => params,
				Err(error) => { yield report_param_event(error); return; },
			};
			if params.session_id.trim().is_empty() || params.device.trim().is_empty() || params.rev.trim().is_empty() || !params.verdict.is_object() {
				yield report_done(Err(MediaFault { code: sf!("invalid_issue_report"), backend: sf!("autoqa"), message: sf!("session_id, device, and rev are required and verdict must be an object") }));
				return;
			}
			if let Err(error) = incoming.interruptable().committed().await {
				yield report_commit_event(error);
				return;
			}
			let now = now_ms();
			let fingerprint = serde_json::json!({
				"session_id": params.session_id,
				"device": params.device,
				"rev": params.rev,
				"verdict": params.verdict,
			})
			.to_string();
			let issue_id = sf!(
				"qa-{}",
				omp_core::Hash32::sum(fingerprint.as_bytes()).to_hex()
			);
			let encoded = serde_json::json!({
				"issue_id": issue_id,
				"device": params.device,
				"rev": params.rev,
				"verdict": params.verdict,
			});
			let redacted = omp_observability::redact::redact_sensitive_credentials(&encoded.to_string());
			let payload_len = match u32::try_from(redacted.len()) {
				Ok(length) => length,
				Err(_) => {
					yield report_done(Err(MediaFault {
						code: sf!("autoqa_payload_too_large"),
						backend: sf!("autoqa"),
						message: sf!("AutoQA payload exceeds the durable frame limit"),
					}));
					return;
				},
			};
			let payload_offset = match self.store.append(
				params.session_id.as_str(),
				"issue_report",
				now,
				redacted.as_bytes(),
			) {
				Ok(offset) => offset.0,
				Err(error) => {
					yield report_done(Err(store_fault(error.to_string())));
					return;
				},
			};
			let issue = StoredIssue {
				id: issue_id.clone(),
				session_id: params.session_id.clone(),
				device: params.device.clone(),
				rev: Some(params.rev.clone()),
				consent: sf!("local_only"),
				created_at_ms: now,
				payload_offset,
				payload_len,
				consent_revision: None,
				attempt_count: 0,
				next_attempt_at_ms: 0,
				terminal: false,
				remote_ack: None,
			};
			if let Err(error) = self.store.store_issue(&issue) {
				yield report_done(Err(store_fault(error.to_string())));
				return;
			}
			let target = format!("{}@{}", issue.device, params.rev);
			yield report_done(Ok(ReportPayload {
				issue_id,
				target: Str::from(target),
				revision: params.rev,
				summary: sf!("A redacted AutoQA report is ready for optional delivery."),
				kind: sf!("issue_report"),
			}));
		}
	}

	fn prompt(&self, view: Result<&ReportPayload, &MediaFault>, _: &PromptCaps) -> Vec<Part> {
		vec![Part::Text {
			text: match view {
				Ok(payload) => {
					Str::from(format!("Filed AutoQA issue {} for {}.", payload.issue_id, payload.target))
				},
				Err(fault) => {
					Str::from(serde_json::to_string(fault).unwrap_or_else(|_| fault.to_string()))
				},
			},
		}]
	}
}

fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(u64::MAX)
}
fn store_fault(message: String) -> MediaFault {
	MediaFault {
		code:    sf!("autoqa_store_failed"),
		backend: sf!("sqlite"),
		message: Str::from(message),
	}
}
const fn media_done(
	result: Result<MediaPayload, MediaFault>,
) -> Ev<MediaUpdate, MediaPayload, MediaFault> {
	Ev::Done(ToolTerminal::Done { result, useless: false })
}
const fn report_done(
	result: Result<ReportPayload, MediaFault>,
) -> Ev<MediaUpdate, ReportPayload, MediaFault> {
	Ev::Done(ToolTerminal::Done { result, useless: false })
}
fn media_param_event(error: ParamError) -> Ev<MediaUpdate, MediaPayload, MediaFault> {
	map_param(error)
}
fn report_param_event(error: ParamError) -> Ev<MediaUpdate, ReportPayload, MediaFault> {
	map_param(error)
}
fn media_commit_event(error: CommitError) -> Ev<MediaUpdate, MediaPayload, MediaFault> {
	map_commit(error)
}
fn report_commit_event(error: CommitError) -> Ev<MediaUpdate, ReportPayload, MediaFault> {
	map_commit(error)
}
fn map_param<U, P, F>(error: ParamError) -> Ev<U, P, F> {
	match error {
		ParamError::Args(issue) => Ev::Args(*issue),
		ParamError::Interrupted(interrupt) => {
			Ev::Aborted(Abort::Interrupted { reason: interrupt.reason })
		},
		ParamError::Protocol(message) => Ev::Args(protocol_issue(message)),
	}
}
fn map_commit<U, P, F>(error: CommitError) -> Ev<U, P, F> {
	match error {
		CommitError::Aborted => Ev::Aborted(Abort::InputDropped),
		CommitError::Interrupted(interrupt) => {
			Ev::Aborted(Abort::Interrupted { reason: interrupt.reason })
		},
		CommitError::Protocol(message) => Ev::Args(protocol_issue(message)),
	}
}
fn protocol_issue(message: Str) -> ArgIssue {
	ArgIssue {
		path:     Vec::new(),
		expected: sf!("one committed JSON argument object"),
		kind:     ArgIssueKind::Protocol,
		example:  None,
		found:    Some(message),
	}
}
#[cfg(test)]
mod tests {
	use tempfile::tempdir;

	use super::*;

	fn media_params() -> MediaParams {
		MediaParams {
			subject:      None,
			action:       None,
			scene:        None,
			composition:  None,
			lighting:     None,
			style:        None,
			text:         None,
			changes:      None,
			aspect_ratio: None,
			image_size:   None,
			input:        None,
			provider:     None,
			voice_id:     None,
			language:     None,
			sample_rate:  None,
			bit_rate:     None,
			output_path:  None,
		}
	}

	#[test]
	fn tts_schema_and_runtime_require_bounded_text_and_output_path() {
		let root = tempdir().expect("root");
		let blobs = BlobHost::open(root.path().join("blobs")).expect("blobs");
		let backend = Arc::new(SearchBridgeHost::new(None));
		let device = tts(backend, blobs, root.path().to_path_buf());
		let schema: Value = serde_json::from_slice(&device.spec().schema).expect("tts schema");
		assert_eq!(schema["required"], serde_json::json!(["i", "text", "output_path"]));
		assert_eq!(
			schema["properties"]
				.as_object()
				.expect("TTS properties")
				.keys()
				.map(String::as_str)
				.collect::<std::collections::BTreeSet<_>>(),
			[
				"bit_rate",
				"i",
				"language",
				"notrunc",
				"output_path",
				"sample_rate",
				"text",
				"voice_id",
			]
				.into_iter()
				.collect()
		);

		let mut params = media_params();
		params.text = Some(Str::new("x".repeat(MAX_SPEECH_CHARS)));
		params.output_path = Some(sf!("speech.wav"));
		assert_eq!(validate_speech_params(&params), Ok(()));

		params.text = Some(Str::new("x".repeat(MAX_SPEECH_CHARS + 1)));
		assert_eq!(
			validate_speech_params(&params)
				.expect_err("overlong speech")
				.code,
			"text_too_long"
		);
		params.text = Some(sf!("hello"));
		params.output_path = None;
		assert_eq!(
			validate_speech_params(&params)
				.expect_err("missing output path")
				.code,
			"output_path_required"
		);
	}

	#[test]
	fn image_schema_and_request_preserve_structured_contract() {
		let root = tempdir().expect("root");
		let blobs = BlobHost::open(root.path().join("blobs")).expect("blobs");
		let backend = Arc::new(SearchBridgeHost::new(None));
		let device = image_gen(backend, blobs, root.path().to_path_buf());
		let schema: Value = serde_json::from_slice(&device.spec().schema).expect("image schema");
		assert_eq!(schema["required"], serde_json::json!(["i", "subject"]));
		assert_eq!(
			schema["properties"]
				.as_object()
				.expect("image properties")
				.keys()
				.map(String::as_str)
				.collect::<std::collections::BTreeSet<_>>(),
			[
				"action",
				"aspect_ratio",
				"changes",
				"composition",
				"i",
				"image_size",
				"input",
				"lighting",
				"notrunc",
				"output_path",
				"provider",
				"scene",
				"style",
				"subject",
				"text",
			]
			.into_iter()
			.collect()
		);

		let mut params = media_params();
		params.subject = Some(sf!("frog"));
		params.action = Some(sf!("jumping"));
		params.scene = Some(sf!("a pond"));
		params.composition = Some(sf!("wide shot"));
		params.changes = Some(vec![sf!("make the frog blue")]);
		params.aspect_ratio = Some(sf!("16:9"));
		params.image_size = Some(sf!("1536x1024"));
		params.input = Some(vec![ImageInput {
			path:      None,
			data:      Some(Str::new(omp_core::base64::encode(b"\x89PNG\r\n\x1a\n").into_string())),
			mime_type: Some(sf!("image/png")),
		}]);
		assert_eq!(validate_image_params(&params), Ok(()));
		let request = image_request(root.path(), &params).expect("image request");
		assert_eq!(
			request.prompt,
			"frog, jumping, a pond. wide shot. Apply these changes: make the frog blue. Preserve all \
			 unspecified details."
		);
		assert_eq!(
			request.size,
			Some(generate_image_request::ImageSize { width: 1536, height: 1024 })
		);
		assert_eq!(request.input_images.len(), 1);
		assert_eq!(request.input_images[0].mime, "image/png");
	}

	#[test]
	fn legacy_media_calls_lift_to_revision_three() {
		let root = tempdir().expect("root");
		let blobs = BlobHost::open(root.path().join("blobs")).expect("blobs");
		let backend = Arc::new(SearchBridgeHost::new(None));
		let image = image_gen(Arc::clone(&backend), blobs.clone(), root.path().to_path_buf());
		let lifted = image
			.lift(&Rev { family: Str::default(), n: 2 }, RecordedCall {
				raw_args: br#"{"prompt":"frog","input_image":"frog.png","format":"png"}"#,
				verdict:  br#"{"kind":"ok","value":{}}"#,
			})
			.expect("legacy image lift");
		let arguments: Value = serde_json::from_slice(&lifted.raw_args).expect("lifted arguments");
		assert_eq!(arguments["subject"], "frog");
		assert_eq!(arguments["input"], serde_json::json!([{"path":"frog.png"}]));
		assert!(arguments.get("prompt").is_none());
		assert!(arguments.get("format").is_none());

		let speech = tts(backend, blobs, root.path().to_path_buf());
		let lifted = speech
			.lift(
				&Rev { family: Str::default(), n: 2 },
				RecordedCall {
					raw_args: br#"{"text":"hello","output_path":"hello.wav","provider":"local","voice":"old","format":"wav"}"#,
					verdict:  br#"{"kind":"ok","value":{}}"#,
				},
			)
			.expect("legacy speech lift");
		let arguments: Value = serde_json::from_slice(&lifted.raw_args).expect("lifted arguments");
		assert_eq!(arguments["text"], "hello");
		assert_eq!(arguments["output_path"], "hello.wav");
		assert!(arguments.get("provider").is_none());
		assert!(arguments.get("voice").is_none());
		assert!(arguments.get("format").is_none());
	}

	#[test]
	fn generated_image_round_trips_as_a_typed_blob_part() {
		let root = tempdir().expect("root");
		let blobs = BlobHost::open(root.path().join("blobs")).expect("blobs");
		let backend = Arc::new(SearchBridgeHost::new(None));
		let device = image_gen(backend, blobs.clone(), root.path().to_path_buf());
		let payload = finish_image(&blobs, None, &thread_pb::Blob {
			hash:   Bytes::new(),
			mime:   "image/png".to_owned(),
			size:   3,
			inline: Bytes::from_static(b"png"),
			detail: blob::Detail::Original as i32,
		})
		.expect("finish image");
		let parts = device.prompt(Ok(&payload), &PromptCaps {
			maximum_parts:      16,
			maximum_text_bytes: 1024,
			media:              true,
			dialect:            Default::default(),
			model_class:        Default::default(),
		});
		let [Part::Blob { blob, alt: None }] = parts.as_slice() else {
			panic!("image prompt must retain a typed blob")
		};
		assert_eq!(blob.media_type, "image/png");
		assert_eq!(blob.byte_len, 3);
		let hash = blob.hash.parse::<omp_core::Hash32>().expect("blob hash");
		assert_eq!(
			blobs
				.get(crate::blobs::BlobId { hash: hash.into_bytes(), size: blob.byte_len })
				.expect("round-trip blob"),
			Bytes::from_static(b"png")
		);
	}

	#[test]
	fn image_input_is_workspace_rooted_capped_and_mime_sniffed() {
		let root = tempdir().expect("root");
		fs::write(root.path().join("image.bin"), b"\x89PNG\r\n\x1a\n").expect("image");
		let image = load_input_image(root.path(), "image.bin").expect("sniff image");
		assert_eq!(image.mime, "image/png");
		assert_eq!(image.inline.as_ref(), b"\x89PNG\r\n\x1a\n");

		fs::write(root.path().join("text.png"), b"not an image").expect("text");
		assert_eq!(
			load_input_image(root.path(), "text.png")
				.expect_err("reject extension-only image")
				.code,
			"input_image_unsupported"
		);
		assert_eq!(
			load_input_image(root.path(), "../outside.png")
				.expect_err("reject parent traversal")
				.code,
			"invalid_input_image"
		);

		let oversized = std::fs::File::create(root.path().join("large.png")).expect("large image");
		oversized
			.set_len(MAX_INPUT_IMAGE_BYTES + 1)
			.expect("sparse oversized image");
		assert_eq!(
			load_input_image(root.path(), "large.png")
				.expect_err("reject oversized image")
				.code,
			"input_image_too_large"
		);
	}

	#[cfg(unix)]
	#[test]
	fn image_input_and_output_refuse_symlink_escape() {
		use std::os::unix::fs::symlink;

		let root = tempdir().expect("root");
		let outside = tempdir().expect("outside");
		fs::write(outside.path().join("image.png"), b"\x89PNG\r\n\x1a\n").expect("outside image");
		symlink(outside.path(), root.path().join("escape")).expect("symlink");
		assert_eq!(
			load_input_image(root.path(), "escape/image.png")
				.expect_err("input symlink escape")
				.code,
			"invalid_input_image"
		);
		assert_eq!(
			OutputTarget::resolve(root.path(), "escape/output.png")
				.expect_err("output symlink escape")
				.code,
			"invalid_output_path"
		);
	}

	#[test]
	fn output_target_atomically_replaces_only_the_contained_file() {
		let root = tempdir().expect("root");
		fs::create_dir(root.path().join("out")).expect("output directory");
		let target = OutputTarget::resolve(root.path(), "out/speech.wav").expect("target");
		assert_eq!(target.write(b"first").expect("first write"), "out/speech.wav");
		assert_eq!(fs::read(root.path().join("out/speech.wav")).expect("first read"), b"first");

		let replacement =
			OutputTarget::resolve(root.path(), "out/speech.wav").expect("replacement target");
		assert_eq!(replacement.write(b"second").expect("replacement write"), "out/speech.wav");
		assert_eq!(
			fs::read(root.path().join("out/speech.wav")).expect("replacement read"),
			b"second"
		);
	}

	#[test]
	fn report_schema_is_closed_and_accepts_structured_verdicts() {
		assert!(
			serde_json::from_value::<ReportParams>(
				serde_json::json!({"session_id":"s","device":"read","rev":"2","verdict":{"ok":false}})
			)
			.is_ok()
		);
		assert!(
			serde_json::from_value::<ReportParams>(
				serde_json::json!({"session_id":"s","device":"read","rev":"2","verdict":{},"extra":true})
			)
			.is_err()
		);
	}

	#[test]
	fn report_store_round_trips_issue_rows() {
		let root = tempdir().unwrap();
		let store = TelemetryIndex::open(root.path(), &root.path().join("telemetry.sqlite")).unwrap();
		let issue = StoredIssue {
			id:                 sf!("i"),
			session_id:         sf!("s"),
			device:             sf!("read"),
			rev:                Some(sf!("2")),
			consent:            sf!("local_only"),
			created_at_ms:      1,
			payload_offset:     0,
			payload_len:        0,
			consent_revision:   None,
			attempt_count:      0,
			next_attempt_at_ms: 0,
			terminal:           false,
			remote_ack:         None,
		};
		store.store_issue(&issue).unwrap();
		assert_eq!(store.issue("i").unwrap(), Some(issue));
	}
}
