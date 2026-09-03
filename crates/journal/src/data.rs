//! Typed JSON payloads for the closed revision-1 kind set.

use omp_core::Str;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::value::RawValue;
use strum::{Display, EnumString, IntoStaticStr};

use crate::{EntryId, blob::BlobRef};

/// `journal@1` genesis payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Genesis {
	/// Journal format version.
	pub version: u32,
	/// Session working directory.
	pub cwd:     Str,
	/// Creation time in the controller's canonical representation.
	pub created: Str,
}

/// `turn.start@1` payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStart {}

/// One user attachment in a `msg.user@1` payload.
///
/// The content-addressed bytes plus the media type pi's
/// `ImageContent.mimeType` carries, so the projection can hand providers a
/// typed media part without reopening the blob. Serialized flat beside the
/// reference: `{"h","n","mime"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
	/// Content-addressed bytes (digest + byte length).
	#[serde(flatten)]
	pub blob: BlobRef,
	/// Declared media type (`image/png`, …).
	pub mime: Str,
}

/// `msg.user@1` payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsgUser {
	/// User-authored text.
	pub text:        Str,
	/// Attached media, positional: `[Image #N]` in `text` names
	/// `attachments[N-1]`.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub attachments: Vec<Attachment>,
}

/// `msg.assistant.start@1` payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsgAssistantStart {
	/// Requested model identifier.
	pub model:    Str,
	/// Provider identifier.
	pub provider: Str,
	/// Resolved route identifier.
	pub route:    Str,
}

/// Operation carried by a `stream@1` entry.
#[derive(
	Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
pub enum StreamOp {
	/// Bind a new stream id to a node property.
	Open,
	/// Append a text delta.
	Append,
	/// Close the stream id.
	Close,
}

/// `stream@1` payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stream {
	/// Session-local stream identity.
	pub sid:  u32,
	/// Stream operation.
	pub op:   StreamOp,
	/// DOM handle bound by an open operation.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub node: Option<u64>,
	/// DOM property bound by an open operation.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub prop: Option<Str>,
	/// Text carried by an append operation.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub text: Option<Str>,
}

/// `msg.assistant.end@1` payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsgAssistantEnd {
	/// Provider stop reason.
	pub stop_reason: Str,
}

/// `tool.call@1` payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
	/// Tool name.
	pub name:    Str,
	/// Tool contract revision.
	pub rev:     u32,
	/// Provider/tool-loop call identity.
	pub call_id: Str,
	/// Model-supplied call intent.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub i:       Option<Str>,
	/// Complete arguments, when they did not arrive through a stream.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub args:    Option<Box<RawValue>>,
	/// Argument stream identity, when arguments arrive incrementally.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub sid:     Option<u32>,
}

/// `tool.update@1` payload: the tool's own typed update JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolUpdate(pub Box<RawValue>);

/// `tool.result@1` terminal payload.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ToolResult {
	/// Successful terminal payload.
	Outcome {
		/// Tool-defined outcome JSON.
		outcome:      Box<RawValue>,
		/// Durable model-facing projection produced by the exact tool revision.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		prompt_parts: Option<Box<RawValue>>,
	},
	/// Failed terminal payload.
	Fault {
		/// Tool-defined fault JSON.
		fault:        Box<RawValue>,
		/// Durable model-facing projection produced by the exact tool revision.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		prompt_parts: Option<Box<RawValue>>,
	},
}

impl<'de> Deserialize<'de> for ToolResult {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		#[derive(Deserialize)]
		#[serde(deny_unknown_fields)]
		struct Wire {
			#[serde(default)]
			outcome:      Option<Box<RawValue>>,
			#[serde(default)]
			fault:        Option<Box<RawValue>>,
			#[serde(default)]
			prompt_parts: Option<Box<RawValue>>,
		}

		let wire = Wire::deserialize(deserializer)?;
		match (wire.outcome, wire.fault) {
			(Some(outcome), None) => Ok(Self::Outcome { outcome, prompt_parts: wire.prompt_parts }),
			(None, Some(fault)) => Ok(Self::Fault { fault, prompt_parts: wire.prompt_parts }),
			_ => Err(de::Error::custom("tool result must contain exactly one of outcome or fault")),
		}
	}
}

/// `turn.receipt@1` payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnReceipt {
	/// Input token count.
	pub tokens_in:     u64,
	/// Output token count.
	pub tokens_out:    u64,
	/// Cost in billionths of a US dollar.
	pub cost_nano_usd: u64,
	/// Prompt-cache tokens read; absent in journals written before the field
	/// existed.
	#[serde(default, skip_serializing_if = "is_zero")]
	pub cache_read:    u64,
	/// Prompt-cache tokens written.
	#[serde(default, skip_serializing_if = "is_zero")]
	pub cache_write:   u64,
	/// Milliseconds from request start to the first streamed token, measured
	/// on the kernel's clock.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub ttft_ms:       Option<u64>,
	/// Milliseconds from request start to completion.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub duration_ms:   Option<u64>,
	/// Provider premium-request units billed for this request in millionths
	/// (`1_000_000` = one premium request; GitHub Copilot `premium_interactions`,
	/// fractional for discounted models); zero for every other route.
	#[serde(default, skip_serializing_if = "is_zero")]
	pub premium_requests_millionths: u64,
}

impl TurnReceipt {
	/// A receipt carrying only token and cost totals.
	#[must_use]
	pub const fn tokens(tokens_in: u64, tokens_out: u64, cost_nano_usd: u64) -> Self {
		Self {
			tokens_in,
			tokens_out,
			cost_nano_usd,
			cache_read: 0,
			cache_write: 0,
			ttft_ms: None,
			duration_ms: None,
			premium_requests_millionths: 0,
		}
	}
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde skip predicate signature")]
const fn is_zero(value: &u64) -> bool {
	*value == 0
}

/// `patch@1` payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Patch {
	/// Serialized array of DOM operations; `omp-dom` owns their Rust type.
	pub ops: Box<RawValue>,
}

/// `compaction@1` payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compaction {
	/// Content-addressed summary.
	pub summary:       BlobRef,
	/// Last entry hidden by the summary.
	pub boundary:      EntryId,
	/// Maintenance method that produced the summary (`auto`, `remote`, `soft`,
	/// `handoff`, `snapcompact`, `shake`, `branch`); absent for legacy
	/// entries, which render as plain compaction.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub method:        Option<Str>,
	/// Estimated context tokens before the step.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tokens_before: Option<u64>,
	/// Estimated context tokens after the step.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tokens_after:  Option<u64>,
	/// Dead-end warning stamped by a progress guard.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub warning:       Option<Str>,
}

impl Compaction {
	/// A compaction with no method or token facts.
	#[must_use]
	pub const fn new(summary: BlobRef, boundary: EntryId) -> Self {
		Self { summary, boundary, method: None, tokens_before: None, tokens_after: None, warning: None }
	}
}

#[cfg(test)]
mod tests {
	use omp_core::Hash32;

	use super::*;

	#[test]
	fn user_attachment_serializes_flat_beside_its_blob_reference() {
		let payload = MsgUser {
			text:        Str::new_static("look [Image #1]"),
			attachments: vec![Attachment {
				blob: BlobRef { hash: Hash32::new([0xab; 32]), size: 5 },
				mime: Str::new_static("image/png"),
			}],
		};
		let json = serde_json::to_string(&payload).unwrap();
		assert_eq!(
			json,
			concat!(
				r#"{"text":"look [Image #1]","attachments":[{"h":""#,
				"abababababababababababababababababababababababababababababababab",
				r#"","n":5,"mime":"image/png"}]}"#
			)
		);
		assert_eq!(serde_json::from_str::<MsgUser>(&json).unwrap(), payload);
		let bare: MsgUser = serde_json::from_str(r#"{"text":"hi"}"#).unwrap();
		assert!(bare.attachments.is_empty());
	}
}
