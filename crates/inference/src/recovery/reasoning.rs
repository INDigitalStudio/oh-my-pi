//! Incremental reasoning-progress and stall detection.

use std::collections::{BTreeSet, VecDeque};

use omp_core::Str;

use super::{
	RecoveryError, Stage,
	repetition::{
		LoopDisposition, LoopEvidence, LoopKind, LoopSignal, OutputVisibility, stable_hash,
	},
};

/// Bounds for reasoning stall detection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReasoningLimits {
	/// Consecutive equivalent deltas that declare a direct repetition loop.
	pub repeated_delta_limit: u32,
	/// Substantial semantic segments required before similarity may declare a stall.
	pub no_progress_limit:    u32,
	/// Maximum retained normalized bytes per delta.
	pub max_delta_bytes:      usize,
}

impl Default for ReasoningLimits {
	fn default() -> Self {
		Self { repeated_delta_limit: 4, no_progress_limit: 12, max_delta_bytes: 16 * 1024 }
	}
}

/// Incremental input to the reasoning guard.
#[derive(Clone, Debug)]
pub struct ReasoningObservation<'a> {
	/// Reasoning text received in this increment.
	pub delta:             &'a str,
	/// An external semantic transition, such as producing answer text or a valid
	/// tool call.
	pub semantic_progress: bool,
	/// Current output visibility at the recovery boundary.
	pub visibility:        OutputVisibility,
}

/// Bounded state machine detecting direct repeats and semantically repetitive
/// reasoning segments.
#[derive(Debug)]
pub struct ReasoningStallGuard {
	limits:        ReasoningLimits,
	last:          Option<(u64, Str)>,
	repeated:      u32,
	input_bytes:   u64,
	pending:       String,
	segments_seen: u32,
	segments:      VecDeque<SemanticSegment>,
}

#[derive(Debug)]
struct SemanticSegment {
	fingerprint: u64,
	shingles:    BTreeSet<u64>,
}

const SEGMENT_CHAR_CAP: usize = 700;
const SEGMENT_MIN_NORMALIZED_BYTES: usize = 60;
const SEGMENT_WINDOW: usize = 16;

impl ReasoningStallGuard {
	/// Creates a reasoning guard with fixed memory bounds.
	pub const fn new(limits: ReasoningLimits) -> Self {
		Self {
			limits,
			last: None,
			repeated: 0,
			input_bytes: 0,
			pending: String::new(),
			segments_seen: 0,
			segments: VecDeque::new(),
		}
	}

	/// Observes one delta and emits at most one stable loop decision.
	pub fn observe(&mut self, observation: ReasoningObservation<'_>) -> Option<LoopSignal> {
		self.input_bytes = self
			.input_bytes
			.saturating_add(observation.delta.len() as u64);
		if observation.semantic_progress {
			self.clear_progress_state();
			return None;
		}
		let direct = normalize_reasoning(observation.delta, self.limits.max_delta_bytes);
		let direct_signal = direct.and_then(|normalized| {
			let fingerprint = stable_hash(normalized.as_bytes());
			let exact_repeat = self.last.as_ref().is_some_and(|(previous_hash, previous)| {
				*previous_hash == fingerprint && previous.as_str() == normalized
			});
			self.repeated = if exact_repeat {
				self.repeated.saturating_add(1)
			} else {
				1
			};
			self.last = Some((fingerprint, Str::new(normalized)));
			(self.repeated >= self.limits.repeated_delta_limit)
				.then_some((fingerprint, self.repeated))
		});
		if let Some((fingerprint, repetitions)) = direct_signal {
			return Some(self.signal(fingerprint, repetitions, observation.visibility));
		}
		self.pending.push_str(observation.delta);
		while let Some(end) = completed_segment_end(&self.pending) {
			let segment = self.pending.drain(..end).collect::<String>();
			trim_segment_separator(&mut self.pending);
			if let Some((fingerprint, repetitions)) = self.consume_segment(&segment) {
				return Some(self.signal(fingerprint, repetitions, observation.visibility));
			}
		}
		while self.pending.len() > SEGMENT_CHAR_CAP {
			let end = floor_char_boundary(&self.pending, SEGMENT_CHAR_CAP);
			let segment = self.pending.drain(..end).collect::<String>();
			if let Some((fingerprint, repetitions)) = self.consume_segment(&segment) {
				return Some(self.signal(fingerprint, repetitions, observation.visibility));
			}
		}
		None
	}

	fn consume_segment(&mut self, raw: &str) -> Option<(u64, u32)> {
		let normalized = normalize_semantic_segment(raw, self.limits.max_delta_bytes)?;
		if normalized.len() < SEGMENT_MIN_NORMALIZED_BYTES {
			return None;
		}
		let fingerprint = stable_hash(normalized.as_bytes());
		let shingles = trigram_shingles(&normalized);
		let cluster = self
			.segments
			.iter()
			.filter(|previous| semantic_similarity(&shingles, &previous.shingles))
			.count()
			.saturating_add(1) as u32;
		self.segments_seen = self.segments_seen.saturating_add(1);
		self.segments.push_back(SemanticSegment { fingerprint, shingles });
		while self.segments.len() > SEGMENT_WINDOW {
			self.segments.pop_front();
		}
		(self.segments_seen >= self.limits.no_progress_limit
			&& cluster >= self.limits.repeated_delta_limit)
			.then_some((fingerprint, cluster))
	}

	fn finish_semantic(&mut self, visibility: OutputVisibility) -> Option<LoopSignal> {
		if self.pending.is_empty() {
			return None;
		}
		let segment = std::mem::take(&mut self.pending);
		self
			.consume_segment(&segment)
			.map(|(fingerprint, repetitions)| self.signal(fingerprint, repetitions, visibility))
	}

	fn signal(
		&self,
		fingerprint: u64,
		repetitions: u32,
		visibility: OutputVisibility,
	) -> LoopSignal {
		LoopSignal {
			evidence: LoopEvidence {
				kind: LoopKind::ReasoningStall,
				fingerprint,
				repetitions,
				input_bytes: self.input_bytes,
			},
			disposition: LoopDisposition::from(visibility),
		}
	}

	fn clear_progress_state(&mut self) {
		self.last = None;
		self.repeated = 0;
		self.pending.clear();
		self.segments_seen = 0;
		self.segments.clear();
	}

	/// Clears attempt-local state while retaining configuration.
	pub fn reset(&mut self) {
		self.clear_progress_state();
		self.input_bytes = 0;
	}
}

fn normalize_reasoning(input: &str, limit: usize) -> Option<String> {
	let mut output = String::with_capacity(input.len().min(limit));
	for word in input.split_ascii_whitespace() {
		if !output.is_empty() {
			output.push(' ');
		}
		output.push_str(word);
		if output.len() > limit {
			return None;
		}
	}
	(!output.is_empty()).then_some(output)
}

fn normalize_semantic_segment(input: &str, limit: usize) -> Option<String> {
	let mut output = String::with_capacity(input.len().min(limit));
	for line in input.lines() {
		let line = line.trim();
		if line.starts_with('#') || is_emphasis_title(line) {
			continue;
		}
		for word in line
			.split(|character: char| !character.is_ascii_alphanumeric())
			.filter(|word| word.bytes().any(|byte| byte.is_ascii_alphabetic()))
		{
			if !output.is_empty() {
				output.push(' ');
			}
			output.extend(word.chars().flat_map(char::to_lowercase));
			if output.len() > limit {
				return None;
			}
		}
	}
	(!output.is_empty()).then_some(output)
}

fn is_emphasis_title(line: &str) -> bool {
	(line.starts_with("**") && line.ends_with("**"))
		|| (line.starts_with("***") && line.ends_with("***"))
}

fn trigram_shingles(normalized: &str) -> BTreeSet<u64> {
	let words: Vec<_> = normalized.split(' ').collect();
	if words.len() < 3 {
		return std::iter::once(iter_shingle_hash(&words)).collect();
	}
	words
		.windows(3)
		.map(iter_shingle_hash)
		.collect()
}

fn iter_shingle_hash(words: &[&str]) -> u64 {
	let mut bytes = Vec::new();
	for word in words {
		bytes.extend_from_slice(word.as_bytes());
		bytes.push(0);
	}
	stable_hash(&bytes)
}

fn semantic_similarity(left: &BTreeSet<u64>, right: &BTreeSet<u64>) -> bool {
	if left.is_empty() || right.is_empty() {
		return false;
	}
	let intersection = left.intersection(right).count();
	let union = left.len().saturating_add(right.len()).saturating_sub(intersection);
	intersection.saturating_mul(5) >= union.saturating_mul(4)
}

fn completed_segment_end(input: &str) -> Option<usize> {
	let bytes = input.as_bytes();
	for first in 0..bytes.len() {
		if bytes[first] != b'\n' {
			continue;
		}
		let mut next = first + 1;
		while next < bytes.len() && matches!(bytes[next], b' ' | b'\t' | b'\r') {
			next += 1;
		}
		if bytes.get(next) == Some(&b'\n') {
			return Some(first);
		}
	}
	None
}

fn trim_segment_separator(input: &mut String) {
	let amount = input
		.bytes()
		.take_while(|byte| byte.is_ascii_whitespace())
		.count();
	input.drain(..amount);
}

fn floor_char_boundary(input: &str, mut end: usize) -> usize {
	while !input.is_char_boundary(end) {
		end -= 1;
	}
	end
}
impl<'a> Stage<ReasoningObservation<'a>, LoopSignal> for ReasoningStallGuard {
	fn push(
		&mut self,
		input: ReasoningObservation<'a>,
		emit: &mut dyn FnMut(LoopSignal),
	) -> Result<(), RecoveryError> {
		if let Some(signal) = self.observe(input) {
			emit(signal);
		}
		Ok(())
	}

	fn finish(&mut self, emit: &mut dyn FnMut(LoopSignal)) -> Result<(), RecoveryError> {
		if let Some(signal) = self.finish_semantic(OutputVisibility::Gated) {
			emit(signal);
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reasoning_stall_obeys_commit_boundary() {
		let limits = ReasoningLimits { repeated_delta_limit: 2, ..ReasoningLimits::default() };
		let mut guard = ReasoningStallGuard::new(limits);
		assert!(
			guard
				.observe(ReasoningObservation {
					delta:             "I should inspect",
					semantic_progress: false,
					visibility:        OutputVisibility::Gated,
				})
				.is_none()
		);
		let gated = guard
			.observe(ReasoningObservation {
				delta:             "I should inspect",
				semantic_progress: false,
				visibility:        OutputVisibility::Gated,
			})
			.unwrap();
		assert_eq!(gated.disposition, LoopDisposition::RetryEligible);
		guard.reset();
		guard.observe(ReasoningObservation {
			delta:             "again",
			semantic_progress: false,
			visibility:        OutputVisibility::Committed,
		});
		let committed = guard
			.observe(ReasoningObservation {
				delta:             "again",
				semantic_progress: false,
				visibility:        OutputVisibility::Committed,
			})
			.unwrap();
		assert_eq!(committed.disposition, LoopDisposition::SurfaceCommitted);
	}

	#[test]
	fn novel_reasoning_segments_do_not_trip_the_guard() {
		let limits = ReasoningLimits {
			repeated_delta_limit: 3,
			no_progress_limit: 4,
			..ReasoningLimits::default()
		};
		let mut guard = ReasoningStallGuard::new(limits);
		for paragraph in [
			"First I will compare the parser boundary with the documented contract and identify which invariant is currently violated.",
			"Next I will inspect the event projection order to determine whether committed output can overtake a recovered tool call.",
			"Then I will trace ownership through the session journal and verify that replay observes the same canonical arguments.",
			"Finally I will review resource limits and make sure incomplete buffers resolve deterministically when the stream finishes.",
			"A separate pass will check receipt evidence so every applied recovery remains attributable to the selected wire policy.",
		] {
			assert!(
				guard
					.observe(ReasoningObservation {
						delta: &format!("{paragraph}\n\n"),
						semantic_progress: false,
						visibility: OutputVisibility::Gated,
					})
					.is_none(),
				"novel reasoning must not be treated as a stall"
			);
		}
	}

	#[test]
	fn repetitive_semantic_segments_trip_after_bounded_evidence() {
		let limits = ReasoningLimits {
			repeated_delta_limit: 3,
			no_progress_limit: 4,
			..ReasoningLimits::default()
		};
		let mut guard = ReasoningStallGuard::new(limits);
		let paragraphs = [
			"I am now carefully checking the implementation to ensure the final result is safe complete correct and ready for delivery with every detail verified.",
			"I am now carefully checking the implementation to ensure the final result is safe complete correct and ready for delivery with all details verified.",
			"I am now carefully checking the implementation to ensure the final result is safe complete correct and ready for delivery while every detail is verified.",
			"I am now carefully checking the implementation to ensure the final result is safe complete correct and ready for delivery and each detail verified.",
		];
		let mut signal = None;
		for paragraph in paragraphs {
			signal = guard.observe(ReasoningObservation {
				delta: &format!("{paragraph}\n\n"),
				semantic_progress: false,
				visibility: OutputVisibility::Gated,
			});
		}
		let signal = signal.expect("repetitive semantic segments must terminate the stall");
		assert_eq!(signal.evidence.kind, LoopKind::ReasoningStall);
		assert_eq!(signal.disposition, LoopDisposition::RetryEligible);
	}

	#[test]
	fn explicit_semantic_progress_breaks_the_stall() {
		let limits = ReasoningLimits { repeated_delta_limit: 2, ..ReasoningLimits::default() };
		let mut guard = ReasoningStallGuard::new(limits);
		guard.observe(ReasoningObservation {
			delta:             "same",
			semantic_progress: false,
			visibility:        OutputVisibility::Gated,
		});
		assert!(
			guard
				.observe(ReasoningObservation {
					delta:             "same",
					semantic_progress: true,
					visibility:        OutputVisibility::Gated,
				})
				.is_none()
		);
		assert!(
			guard
				.observe(ReasoningObservation {
					delta:             "same",
					semantic_progress: false,
					visibility:        OutputVisibility::Gated,
				})
				.is_none()
		);
	}
}
