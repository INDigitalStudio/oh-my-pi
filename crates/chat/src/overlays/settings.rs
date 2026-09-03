//! Curated `/settings` selector over convars.
//!
//! Values, types, validation, and persistence remain owned by `omp_con`.
//! This panel consumes only explicit product UI metadata and never derives a
//! visible row, label, tab, or group from a variable name.

use std::fmt::Write as _;

use omp_con::{
	Ctx, DynamicUiWidget, RegItem, SETTING_TABS, SettingTab, TypeSpec, UiCondition, UiOption,
	UiSpec, UiValueCodec, UiWidget, Value, ValueKind, VarFlags, builtin_ui_entries,
};
use omp_core::{Str, StrMut, sf};
use omp_tui::{
	Component, Frame, IntoComponent as _, Key, MouseReport, Prop, Size, Ui, UiContext, UiEvent,
	cell_width, components::Tabs, dom,
};

use super::{Panel, PanelAnchor, PanelCx, PanelEvent};

const FOOTER: &str = "↑/↓ navigate · ←/→ tab · Enter change · type to search · Esc close";
const TEXT_FOOTER: &str = "Enter apply · Esc cancel · Ctrl+U clear";
const CHOICE_FOOTER: &str = "↑/↓ choose · Enter apply · Esc cancel";
const MULTI_FOOTER: &str = "↑/↓ choose · Space toggle · ←/→ reorder · Enter apply · Esc cancel";
const EMPTY: &str = "(empty)";
const CHROME_ROWS: u16 = 10;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Choice {
	value:       Str,
	label:       Str,
	description: Str,
}

impl From<&UiOption> for Choice {
	fn from(option: &UiOption) -> Self {
		Self {
			value:       Str::new_static(option.value),
			label:       Str::new_static(option.label),
			description: Str::new_static(option.description),
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RowWidget {
	Boolean,
	Enum(Vec<Str>),
	Submenu(Vec<Choice>),
	Text { secret: bool },
	MultiSelect { options: Vec<Choice>, ordered: bool },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RowValue {
	Boolean(bool),
	Scalar(Str),
	Multi(Vec<Str>),
	Text(Str),
}

/// One curated editable convar. `convar` is command metadata and is never a
/// display label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingRow {
	convar:        Str,
	pi_path:       Option<Str>,
	label:         Str,
	description:   Str,
	warning:       Option<Str>,
	tab:           SettingTab,
	group:         Str,
	widget:        RowWidget,
	value:         RowValue,
	default:       RowValue,
	value_kind:    ValueKind,
	codec:         UiValueCodec,
	condition:     Option<UiCondition>,
	condition_met: bool,
}

impl SettingRow {
	fn changed(&self) -> bool {
		self.value != self.default
	}

	fn display(&self) -> Str {
		match (&self.widget, &self.value) {
			(RowWidget::Boolean, RowValue::Boolean(value)) => {
				Str::new_static(if *value { "On" } else { "Off" })
			},
			(RowWidget::Submenu(options), RowValue::Scalar(value)) => options
				.iter()
				.find(|option| option.value == *value)
				.map_or_else(|| value.clone(), |option| option.label.clone()),
			(RowWidget::MultiSelect { options, .. }, RowValue::Multi(values)) => {
				if values.is_empty() {
					return Str::new_static(EMPTY);
				}
				let mut text = StrMut::new("");
				for (index, value) in values.iter().enumerate() {
					if index > 0 {
						text.push_str(", ");
					}
					let label = options
						.iter()
						.find(|option| option.value == *value)
						.map_or(value.as_str(), |option| option.label.as_str());
					text.push_str(label);
				}
				text.freeze()
			},
			(_, RowValue::Scalar(value) | RowValue::Text(value)) if value.is_empty() => {
				Str::new_static(EMPTY)
			},
			(_, RowValue::Scalar(value) | RowValue::Text(value)) => value.clone(),
			_ => Str::new_static(EMPTY),
		}
	}

	fn editable(&self) -> String {
		match &self.value {
			RowValue::Text(value) | RowValue::Scalar(value) => value.to_string(),
			RowValue::Boolean(value) => value.to_string(),
			RowValue::Multi(values) => values.iter().map(Str::as_str).collect::<Vec<_>>().join(" "),
		}
	}
}

fn static_widget(widget: UiWidget) -> RowWidget {
	match widget {
		UiWidget::Boolean => RowWidget::Boolean,
		UiWidget::Enum(values) => {
			RowWidget::Enum(values.iter().copied().map(Str::new_static).collect())
		},
		UiWidget::Submenu(options) => RowWidget::Submenu(options.iter().map(Choice::from).collect()),
		UiWidget::Text { secret } => RowWidget::Text { secret },
		UiWidget::MultiSelect { options, ordered } => {
			RowWidget::MultiSelect { options: options.iter().map(Choice::from).collect(), ordered }
		},
	}
}

fn dynamic_widget(widget: &DynamicUiWidget, ty: &TypeSpec) -> RowWidget {
	match widget {
		DynamicUiWidget::Auto => match ty.kind {
			ValueKind::Bool => RowWidget::Boolean,
			ValueKind::Enum => {
				RowWidget::Enum(ty.variants.iter().copied().map(Str::new_static).collect())
			},
			_ => RowWidget::Text { secret: false },
		},
		DynamicUiWidget::Submenu(options) => RowWidget::Submenu(
			options
				.iter()
				.map(|option| Choice {
					value:       option.value.clone(),
					label:       option.label.clone(),
					description: option.description.clone(),
				})
				.collect(),
		),
		DynamicUiWidget::MultiSelect { options, ordered } => RowWidget::MultiSelect {
			options: options
				.iter()
				.map(|option| Choice {
					value:       option.value.clone(),
					label:       option.label.clone(),
					description: option.description.clone(),
				})
				.collect(),
			ordered: *ordered,
		},
	}
}

fn decimal(value: f64) -> Str {
	if value.fract().abs() < f64::EPSILON {
		sf!("{value:.0}")
	} else {
		let mut text = sf!("{value:.6}").to_string();
		while text.ends_with('0') {
			text.pop();
		}
		if text.ends_with('.') {
			text.pop();
		}
		Str::new(text)
	}
}

fn span_units(value: &Value, unit_ms: bool) -> Str {
	let Value::Duration(span) = value else {
		return Str::new(value.to_string());
	};
	let Some(duration) = span.as_finite() else {
		return Str::new_static("0");
	};
	let milliseconds = duration.to_std().map_or(0, |duration| duration.as_millis());
	if unit_ms {
		sf!("{milliseconds}")
	} else {
		sf!("{}", milliseconds / 1000)
	}
}

fn project_value(codec: UiValueCodec, widget: &RowWidget, value: &Value) -> RowValue {
	match codec {
		UiValueCodec::InvertedBoolean => RowValue::Boolean(!value.as_bool().unwrap_or(false)),
		UiValueCodec::OnOffBoolean => {
			RowValue::Scalar(Str::new_static(if value.as_bool().unwrap_or(false) {
				"on"
			} else {
				"off"
			}))
		},
		UiValueCodec::IsolationEnabled => {
			RowValue::Boolean(value.as_str().is_some_and(|value| value != "none"))
		},
		UiValueCodec::Kibibytes => {
			let bytes = value.as_int().map_or(0.0, |value| value as f64);
			RowValue::Scalar(decimal(bytes / 1024.0))
		},
		UiValueCodec::PercentFraction => {
			let fraction = value.as_float().unwrap_or(0.0);
			RowValue::Scalar(decimal(fraction * 100.0))
		},
		UiValueCodec::SecondsDuration => RowValue::Scalar(span_units(value, false)),
		UiValueCodec::MillisecondsDuration => RowValue::Scalar(span_units(value, true)),
		UiValueCodec::Identity => match (widget, value) {
			(RowWidget::Boolean, Value::Bool(value)) => RowValue::Boolean(*value),
			(RowWidget::MultiSelect { .. }, Value::List(values)) => RowValue::Multi(
				values
					.iter()
					.filter_map(|value| value.as_str().map(Str::new))
					.collect(),
			),
			(RowWidget::Text { .. }, Value::Str(value)) => RowValue::Text(value.clone()),
			(RowWidget::Text { .. }, value) => RowValue::Text(Str::new(value.to_string())),
			(_, value) => RowValue::Scalar(
				value
					.as_str()
					.map_or_else(|| Str::new(value.to_string()), Str::new),
			),
		},
	}
}

fn static_row(con: &Ctx, ui: &UiSpec) -> Option<SettingRow> {
	let RegItem::Var(spec) = con.find(ui.convar)? else {
		return None;
	};
	if !spec.flags.contains(VarFlags::ARCHIVE) {
		return None;
	}
	let widget = static_widget(ui.widget);
	let value = con.get(spec.name).unwrap_or_else(|| (spec.default)());
	let default = (spec.default)();
	Some(SettingRow {
		convar: Str::new_static(ui.convar),
		pi_path: Some(Str::new_static(ui.pi_path)),
		label: Str::new_static(ui.label),
		description: Str::new_static(ui.description),
		warning: ui.warning.map(Str::new_static),
		tab: ui.tab,
		group: Str::new_static(ui.group),
		value: project_value(ui.codec, &widget, &value),
		default: project_value(ui.codec, &widget, &default),
		value_kind: spec.ty.kind,
		widget,
		codec: ui.codec,
		condition: ui.condition,
		condition_met: ui.condition.is_none_or(|condition| condition.visible(con)),
	})
}

fn dynamic_row(con: &Ctx, spec: &omp_con::DynamicVarSpec) -> Option<SettingRow> {
	let ui = spec.ui.as_ref()?;
	if !spec.flags.contains(VarFlags::ARCHIVE) || !ui.is_valid(spec.name.as_str()) {
		return None;
	}
	let widget = dynamic_widget(&ui.widget, spec.ty);
	let value = con
		.get(spec.name.as_str())
		.unwrap_or_else(|| spec.default.clone());
	Some(SettingRow {
		convar: spec.name.clone(),
		pi_path: None,
		label: ui.label.clone(),
		description: ui.description.clone(),
		warning: ui.warning.clone(),
		tab: ui.tab,
		group: ui.group.clone(),
		value: project_value(UiValueCodec::Identity, &widget, &value),
		default: project_value(UiValueCodec::Identity, &widget, &spec.default),
		value_kind: spec.ty.kind,
		widget,
		codec: UiValueCodec::Identity,
		condition: None,
		condition_met: true,
	})
}

/// Curated built-in rows plus extension rows whose admitted manifest carries
/// equivalent typed UI metadata. ARCHIVE alone never creates a row.
#[must_use]
pub fn settings_rows(con: &Ctx) -> Vec<SettingRow> {
	builtin_ui_entries()
		.filter_map(|ui| static_row(con, ui))
		.chain(con.dynamic_vars().filter_map(|spec| dynamic_row(con, spec)))
		.collect()
}

fn condition_visible(condition: UiCondition, rows: &[SettingRow], fallback: bool) -> bool {
	let scalar = |name: &str| {
		rows
			.iter()
			.find(|row| row.convar == name)
			.and_then(|row| match &row.value {
				RowValue::Boolean(value) => {
					Some(Str::new_static(if *value { "true" } else { "false" }))
				},
				RowValue::Scalar(value) | RowValue::Text(value) => Some(value.clone()),
				RowValue::Multi(_) => None,
			})
	};
	match condition {
		UiCondition::UsageAwareFallbackEnabled => scalar("ai_retry_usage_aware_fallback")
			.is_some_and(|value| matches!(value.as_str(), "true" | "on")),
		UiCondition::MnemopiActive => {
			scalar("ai_memory_backend").is_some_and(|value| value == "mnemopi")
		},
		UiCondition::AutoThinkingActive => {
			scalar("ai_default_thinking").is_some_and(|value| value == "auto")
		},
		UiCondition::UnexpectedStopSmart => {
			scalar("ai_features_unexpected_stop_detection").map_or(fallback, |value| value == "smart")
		},
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Item {
	TabHeader(SettingTab),
	GroupHeader { tab: SettingTab, group: Str },
	Row(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Editor {
	Text(String),
	Submenu { cursor: usize },
	Multi { cursor: usize, selected: Vec<Str>, ordered: bool },
}

/// Retained curated settings selector.
pub struct SettingsPanel {
	rows:      Vec<SettingRow>,
	tab:       usize,
	query:     String,
	items:     Vec<Item>,
	selected:  usize,
	scroll:    usize,
	list_rows: usize,
	editor:    Option<Editor>,
	ui:        Ui,
	ctx:       UiContext,
	width:     u16,
	height:    u16,
}

impl SettingsPanel {
	/// Opens the selector over explicit product metadata.
	pub fn open(cx: &PanelCx<'_>) -> Result<Self, Str> {
		let rows = settings_rows(cx.con);
		if rows.is_empty() {
			return Err(Str::new_static("No curated settings are registered"));
		}
		Ok(Self::from_rows(rows, cx.ui))
	}

	#[must_use]
	fn from_rows(rows: Vec<SettingRow>, ctx: &UiContext) -> Self {
		let mut panel = Self {
			rows,
			tab: 0,
			query: String::new(),
			items: Vec::new(),
			selected: 0,
			scroll: 0,
			list_rows: 10,
			editor: None,
			ui: Ui::from_root(dom! { <col/> }, 80, ctx.clone()),
			ctx: ctx.clone(),
			width: 80,
			height: 24,
		};
		panel.reflow_items();
		panel.rebuild();
		panel
	}

	fn tab(&self) -> SettingTab {
		SETTING_TABS[self.tab].tab
	}

	fn selected(&self) -> Option<&SettingRow> {
		match self.items.get(self.selected)? {
			Item::Row(index) => self.rows.get(*index),
			Item::TabHeader(_) | Item::GroupHeader { .. } => None,
		}
	}

	fn row_visible(&self, row: &SettingRow) -> bool {
		row.condition
			.is_none_or(|condition| condition_visible(condition, &self.rows, row.condition_met))
	}

	fn matching_indices(&self, tab: SettingTab, group: &str) -> Vec<usize> {
		let query = self.query.to_ascii_lowercase();
		self
			.rows
			.iter()
			.enumerate()
			.filter(|(_, row)| row.tab == tab && row.group == group && self.row_visible(row))
			.filter(|(_, row)| {
				query.is_empty()
					|| row.label.to_ascii_lowercase().contains(query.as_str())
					|| row
						.description
						.to_ascii_lowercase()
						.contains(query.as_str())
			})
			.map(|(index, _)| index)
			.collect()
	}

	fn reflow_items(&mut self) {
		self.items.clear();
		let searching = !self.query.is_empty();
		for tab in SETTING_TABS {
			if !searching && tab.tab != self.tab() {
				continue;
			}
			let mut tab_added = false;
			for group in tab.groups {
				let indices = self.matching_indices(tab.tab, group);
				if indices.is_empty() {
					continue;
				}
				if searching && !tab_added {
					self.items.push(Item::TabHeader(tab.tab));
					tab_added = true;
				}
				self
					.items
					.push(Item::GroupHeader { tab: tab.tab, group: Str::new_static(group) });
				self.items.extend(indices.into_iter().map(Item::Row));
			}
		}
		self.selected = self
			.items
			.iter()
			.position(|item| matches!(item, Item::Row(_)))
			.unwrap_or(0);
		self.scroll = 0;
		self.clamp_scroll();
		self.sync_tab_to_selection();
	}

	fn clamp_scroll(&mut self) {
		if self.selected < self.scroll {
			self.scroll = self.selected;
		} else if self.selected >= self.scroll + self.list_rows {
			self.scroll = self.selected + 1 - self.list_rows;
		}
		self.scroll = self
			.scroll
			.min(self.items.len().saturating_sub(self.list_rows));
	}

	fn move_selection(&mut self, delta: isize) -> bool {
		if self.items.is_empty() || delta == 0 {
			return false;
		}
		let mut next = self.selected;
		let mut last = self.selected;
		let mut moved = 0;
		while moved < delta.unsigned_abs() {
			let Some(candidate) = next.checked_add_signed(delta.signum()) else {
				break;
			};
			if candidate >= self.items.len() {
				break;
			}
			next = candidate;
			if matches!(self.items[next], Item::Row(_)) {
				last = next;
				moved += 1;
			}
		}
		if last == self.selected {
			return false;
		}
		self.selected = last;
		self.clamp_scroll();
		true
	}

	fn switch_tab(&mut self, delta: isize) {
		let len = SETTING_TABS.len() as isize;
		self.tab = ((self.tab as isize + delta).rem_euclid(len)) as usize;
		if self.query.is_empty() {
			self.reflow_items();
		} else if let Some(index) = self
			.items
			.iter()
			.position(|item| matches!(item, Item::TabHeader(tab) if *tab == self.tab()))
		{
			self.selected = self.items[index + 1..]
				.iter()
				.position(|item| matches!(item, Item::Row(_)))
				.map_or(index, |offset| index + 1 + offset);
			self.clamp_scroll();
		}
	}

	fn sync_tab_to_selection(&mut self) {
		if let Some(row) = self.selected()
			&& let Some(index) = SETTING_TABS.iter().position(|tab| tab.tab == row.tab)
		{
			self.tab = index;
		}
	}

	fn select_tab(&mut self, tab: usize) {
		if tab >= SETTING_TABS.len() || tab == self.tab {
			return;
		}
		self.tab = tab;
		if self.query.is_empty() {
			self.reflow_items();
		} else {
			self.switch_tab(0);
		}
		self.rebuild();
	}

	fn sync_pointer_tab(&mut self) {
		let values = self.ui.values();
		let Some(label) = values.get("settings-tabs").and_then(|value| value.as_str()) else {
			return;
		};
		if let Some(index) = SETTING_TABS.iter().position(|tab| tab.label == label) {
			self.select_tab(index);
		}
	}

	fn end_search(&mut self) {
		let keep = self
			.selected()
			.map(|row| row.pi_path.clone().unwrap_or_else(|| row.convar.clone()));
		self.query.clear();
		self.reflow_items();
		if let Some(keep) = keep
			&& let Some(index) = self.items.iter().position(|item| matches!(item, Item::Row(row) if self.rows[*row].pi_path.as_ref().unwrap_or(&self.rows[*row].convar) == &keep))
		{
			self.selected = index;
			self.clamp_scroll();
		}
	}

	fn activate(&mut self) -> PanelEvent {
		let Some(Item::Row(index)) = self.items.get(self.selected).cloned() else {
			return PanelEvent::Consumed;
		};
		match &self.rows[index].widget {
			RowWidget::Boolean => {
				let RowValue::Boolean(value) = self.rows[index].value else {
					return PanelEvent::Consumed;
				};
				self.commit(index, RowValue::Boolean(!value))
			},
			RowWidget::Enum(values) => {
				if values.is_empty() {
					return PanelEvent::Consumed;
				}
				let current = match &self.rows[index].value {
					RowValue::Scalar(value) => value,
					_ => return PanelEvent::Consumed,
				};
				let next = values
					.iter()
					.position(|value| value == current)
					.map_or(0, |at| (at + 1) % values.len());
				self.commit(index, RowValue::Scalar(values[next].clone()))
			},
			RowWidget::Submenu(options) => {
				let current = match &self.rows[index].value {
					RowValue::Scalar(value) => value,
					_ => return PanelEvent::Consumed,
				};
				let cursor = options
					.iter()
					.position(|option| option.value == *current)
					.unwrap_or(0);
				self.editor = Some(Editor::Submenu { cursor });
				self.rebuild();
				PanelEvent::Consumed
			},
			RowWidget::Text { .. } => {
				self.editor = Some(Editor::Text(self.rows[index].editable()));
				self.rebuild();
				PanelEvent::Consumed
			},
			RowWidget::MultiSelect { ordered, .. } => {
				let selected = match &self.rows[index].value {
					RowValue::Multi(value) => value.clone(),
					_ => Vec::new(),
				};
				self.editor = Some(Editor::Multi { cursor: 0, selected, ordered: *ordered });
				self.rebuild();
				PanelEvent::Consumed
			},
		}
	}

	fn command_value(row: &SettingRow, value: &RowValue) -> Result<Str, Str> {
		match (row.codec, value) {
			(UiValueCodec::InvertedBoolean, RowValue::Boolean(value)) => {
				Ok(Str::new_static(if *value { "false" } else { "true" }))
			},
			(UiValueCodec::OnOffBoolean, RowValue::Scalar(value)) => {
				Ok(Str::new_static(if value == "on" { "true" } else { "false" }))
			},
			(UiValueCodec::IsolationEnabled, RowValue::Boolean(value)) => {
				Ok(Str::new_static(if *value { "auto" } else { "none" }))
			},
			(UiValueCodec::Kibibytes, RowValue::Scalar(value)) => value
				.parse::<f64>()
				.map(|value| sf!("{:.0}", value * 1024.0))
				.map_err(|_| Str::new_static("Invalid kilobyte value")),
			(UiValueCodec::PercentFraction, RowValue::Scalar(value)) => {
				if value == "default" {
					return Ok(Str::new_static("0.8"));
				}
				value
					.parse::<f64>()
					.map(|value| decimal(value / 100.0))
					.map_err(|_| Str::new_static("Invalid percent value"))
			},
			(UiValueCodec::SecondsDuration, RowValue::Scalar(value)) => Ok(if value == "0" {
				Str::new_static("never")
			} else {
				sf!("{value}s")
			}),
			(UiValueCodec::MillisecondsDuration, RowValue::Scalar(value)) => Ok(if value == "0" {
				Str::new_static("never")
			} else {
				sf!("{value}ms")
			}),
			(UiValueCodec::Identity, RowValue::Boolean(value)) => {
				Ok(Str::new_static(if *value { "true" } else { "false" }))
			},
			(UiValueCodec::Identity, RowValue::Scalar(value)) => Ok(value.clone()),
			(UiValueCodec::Identity, RowValue::Text(value)) if row.value_kind == ValueKind::Str => {
				Ok(Str::new(Value::Str(value.clone()).to_string()))
			},
			(UiValueCodec::Identity, RowValue::Text(value)) => Ok(value.clone()),
			(UiValueCodec::Identity, RowValue::Multi(values)) => {
				let mut text = StrMut::new("[");
				for (index, value) in values.iter().enumerate() {
					if index > 0 {
						text.push(' ')
					}
					let _ = write!(text, "{}", Value::Str(value.clone()));
				}
				text.push(']');
				Ok(text.freeze())
			},
			_ => Err(Str::new_static("Setting value does not match its UI control")),
		}
	}

	fn commit(&mut self, index: usize, value: RowValue) -> PanelEvent {
		if self.rows[index].value == value {
			self.editor = None;
			self.rebuild();
			return PanelEvent::Consumed;
		}
		let command = match Self::command_value(&self.rows[index], &value) {
			Ok(command) => command,
			Err(error) => return PanelEvent::Notice(error),
		};
		self.rows[index].value = value;
		self.editor = None;
		self.reflow_items();
		self.rebuild();
		PanelEvent::Run(sf!("{} {command}; writecfg", self.rows[index].convar))
	}

	fn editor_key(&mut self, key: Key) -> PanelEvent {
		let Some(Item::Row(index)) = self.items.get(self.selected).cloned() else {
			return PanelEvent::Consumed;
		};
		match self.editor.as_mut() {
			Some(Editor::Text(buffer)) => match key {
				Key::Esc => {
					self.editor = None;
					self.rebuild();
					PanelEvent::Consumed
				},
				Key::Enter => {
					let value = RowValue::Text(Str::new(std::mem::take(buffer).trim()));
					self.commit(index, value)
				},
				Key::Backspace => {
					buffer.pop();
					self.rebuild();
					PanelEvent::Consumed
				},
				Key::Space => {
					buffer.push(' ');
					self.rebuild();
					PanelEvent::Consumed
				},
				Key::Ctrl('u') => {
					buffer.clear();
					self.rebuild();
					PanelEvent::Consumed
				},
				Key::Ctrl('w') => {
					buffer.truncate(buffer.trim_end().len());
					buffer.truncate(buffer.rfind(' ').map_or(0, |at| at + 1));
					self.rebuild();
					PanelEvent::Consumed
				},
				Key::Char(character) if !character.is_control() => {
					buffer.push(character);
					self.rebuild();
					PanelEvent::Consumed
				},
				_ => PanelEvent::Consumed,
			},
			Some(Editor::Submenu { cursor }) => {
				let RowWidget::Submenu(options) = &self.rows[index].widget else {
					return PanelEvent::Consumed;
				};
				match key {
					Key::Esc => {
						self.editor = None;
						self.rebuild();
						PanelEvent::Consumed
					},
					Key::Up => {
						*cursor = cursor.saturating_sub(1);
						self.rebuild();
						PanelEvent::Consumed
					},
					Key::Down => {
						*cursor = (*cursor + 1).min(options.len().saturating_sub(1));
						self.rebuild();
						PanelEvent::Consumed
					},
					Key::Enter if !options.is_empty() => {
						let value = options[*cursor].value.clone();
						self.editor = None;
						self.commit(index, RowValue::Scalar(value))
					},
					_ => PanelEvent::Consumed,
				}
			},
			Some(Editor::Multi { cursor, selected, ordered }) => {
				let RowWidget::MultiSelect { options, .. } = &self.rows[index].widget else {
					return PanelEvent::Consumed;
				};
				match key {
					Key::Esc => {
						self.editor = None;
						self.rebuild();
						PanelEvent::Consumed
					},
					Key::Up => {
						*cursor = cursor.saturating_sub(1);
						self.rebuild();
						PanelEvent::Consumed
					},
					Key::Down => {
						*cursor = (*cursor + 1).min(options.len().saturating_sub(1));
						self.rebuild();
						PanelEvent::Consumed
					},
					Key::Space if !options.is_empty() => {
						let value = &options[*cursor].value;
						if let Some(at) = selected.iter().position(|item| item == value) {
							selected.remove(at);
						} else {
							selected.push(value.clone());
						}
						self.rebuild();
						PanelEvent::Consumed
					},
					Key::Left | Key::Right if *ordered && !options.is_empty() => {
						let value = &options[*cursor].value;
						if let Some(at) = selected.iter().position(|item| item == value) {
							let next = if key == Key::Left {
								at.saturating_sub(1)
							} else {
								(at + 1).min(selected.len() - 1)
							};
							selected.swap(at, next);
						}
						self.rebuild();
						PanelEvent::Consumed
					},
					Key::Enter => {
						let value = selected.clone();
						self.editor = None;
						self.commit(index, RowValue::Multi(value))
					},
					_ => PanelEvent::Consumed,
				}
			},
			None => PanelEvent::Consumed,
		}
	}

	fn rebuild(&mut self) {
		if self.editor.is_some() {
			self.rebuild_editor();
			return;
		}
		let inner = usize::from(self.width.saturating_sub(4).max(20));
		let list_rows = usize::from(self.height.saturating_sub(CHROME_ROWS).max(3));
		if list_rows != self.list_rows {
			self.list_rows = list_rows;
			self.clamp_scroll();
		}
		let label_width = self
			.rows
			.iter()
			.map(|row| usize::from(cell_width(&row.label)))
			.max()
			.unwrap_or(8)
			.clamp(8, inner.saturating_sub(24).max(8));
		let mut list = self
			.items
			.iter()
			.enumerate()
			.skip(self.scroll)
			.take(self.list_rows)
			.map(|(index, item)| self.list_row(item, index == self.selected, label_width))
			.collect::<Vec<_>>();
		let empty = self.items.is_empty();
		let shown = list.len() + usize::from(empty);
		list.extend(
			std::iter::repeat_with(|| dom! { <text>{" "}</text> }.into_component())
				.take(self.list_rows.saturating_sub(shown)),
		);
		let searching = !self.query.is_empty();
		let query = Str::new(self.query.as_str());
		let description = self
			.selected()
			.map(|row| row.description.clone())
			.unwrap_or_default();
		let warning = self.selected().and_then(|row| row.warning.clone());
		let mut tabs = Tabs::new().with_str(Prop::Id, "settings-tabs");
		for tab in SETTING_TABS {
			tabs = tabs.pane_icon(tab.icon, tab.label, dom! { <col/> });
		}
		let tabs = tabs.select(self.tab as u16);
		self.ui = Ui::from_root(
			dom! {
				<box border=round title="Settings">
					<col>
						{tabs}
						if searching { <row gap=1><text fg=muted>{"Search:"}</text><row><text>{query}</text><text fg=accent>{"_"}</text></row></row> }
						else { <text fg=muted>{"Type to search labels and descriptions"}</text> }
						<text>{" "}</text>
						if empty { <text fg=muted truncate>{"  No settings match."}</text> }
						for row in list { {row} }
						<hr border=round/>
						<text fg=muted truncate>{description}</text>
						if let Some(warning) = warning { <text fg=warn truncate>{warning}</text> }
						else { <text>{" "}</text> }
						<text fg=muted truncate>{FOOTER}</text>
					</col>
				</box>
			},
			self.width,
			self.ctx.clone(),
		);
	}

	fn rebuild_editor(&mut self) {
		let Some(row) = self.selected() else { return };
		let title = row.label.clone();
		let description = row.description.clone();
		let (lines, footer): (Vec<Box<dyn Component>>, &'static str) = match self
			.editor
			.as_ref()
			.expect("checked")
		{
			Editor::Text(buffer) => {
				let text = if matches!(row.widget, RowWidget::Text { secret: true }) {
					"•".repeat(buffer.chars().count())
				} else {
					buffer.clone()
				};
				(
					vec![
						dom! { <row><text fg=accent>{text}</text><text fg=accent>{"_"}</text></row> }
							.into_component(),
					],
					TEXT_FOOTER,
				)
			},
			Editor::Submenu { cursor } => {
				let RowWidget::Submenu(options) = &row.widget else {
					return;
				};
				(options.iter().enumerate().map(|(index, option)| {
					let marker = if index == *cursor { "›" } else { " " };
					let selected = matches!(&row.value, RowValue::Scalar(value) if *value == option.value);
					let check = if selected { "●" } else { "○" };
					let copy = if option.description.is_empty() { option.label.clone() } else { sf!("{} — {}", option.label, option.description) };
					dom! { <row gap=1><text fg=accent>{marker}</text><text fg=muted>{check}</text><text truncate>{copy}</text></row> }.into_component()
				}).collect(), CHOICE_FOOTER)
			},
			Editor::Multi { cursor, selected, ordered } => {
				let RowWidget::MultiSelect { options, .. } = &row.widget else {
					return;
				};
				(options.iter().enumerate().map(|(index, option)| {
					let marker = if index == *cursor { "›" } else { " " };
					let at = selected.iter().position(|value| *value == option.value);
					let check = at.map_or_else(|| Str::new_static("○"), |at| if *ordered { sf!("{}.", at + 1) } else { Str::new_static("●") });
					let copy = if option.description.is_empty() { option.label.clone() } else { sf!("{} — {}", option.label, option.description) };
					dom! { <row gap=1><text fg=accent>{marker}</text><text fg=muted>{check}</text><text truncate>{copy}</text></row> }.into_component()
				}).collect(), MULTI_FOOTER)
			},
		};
		self.ui = Ui::from_root(
			dom! {
				<box border=round title="Settings">
					<col>
						<text bold fg=accent>{title}</text>
						<text fg=muted>{description}</text>
						<text>{" "}</text>
						for line in lines { {line} }
						<text>{" "}</text>
						<text fg=muted>{footer}</text>
					</col>
				</box>
			},
			self.width,
			self.ctx.clone(),
		);
	}

	fn list_row(&self, item: &Item, selected: bool, label_width: usize) -> Box<dyn Component> {
		match item {
			Item::TabHeader(tab) => {
				let label = SETTING_TABS
					.iter()
					.find(|spec| spec.tab == *tab)
					.map_or("", |spec| spec.label);
				dom! { <row gap=1><text bold fg=accent>{label}</text></row> }.into_component()
			},
			Item::GroupHeader { group, .. } => {
				dom! { <row gap=1><text bold fg=muted>{group.clone()}</text></row> }.into_component()
			},
			Item::Row(index) => {
				let row = &self.rows[*index];
				let marker = if row.changed() { "●" } else { " " };
				let label = pad(&row.label, label_width);
				let value = row.display();
				if selected {
					dom! { <row gap=1 bg=surface><text fg=accent>{marker}</text><pre bold fg=accent>{label}</pre><text fg=accent truncate>{value}</text></row> }.into_component()
				} else {
					dom! { <row gap=1><text fg=accent>{marker}</text><pre>{label}</pre><text fg=muted truncate>{value}</text></row> }.into_component()
				}
			},
		}
	}
}

fn pad(label: &str, width: usize) -> Str {
	let used = usize::from(cell_width(label));
	if used >= width {
		return Str::new(label);
	}
	let mut text = String::with_capacity(width);
	text.push_str(label);
	text.extend(std::iter::repeat_n(' ', width - used));
	Str::new(text)
}

impl Panel for SettingsPanel {
	fn id(&self) -> &'static str {
		"settings"
	}

	fn anchor(&self) -> PanelAnchor {
		PanelAnchor::Full
	}

	fn key(&mut self, key: Key) -> PanelEvent {
		if self.editor.is_some() {
			return self.editor_key(key);
		}
		match key {
			Key::Esc if self.query.is_empty() => PanelEvent::Close,
			Key::Esc => {
				self.end_search();
				self.rebuild();
				PanelEvent::Consumed
			},
			Key::Tab | Key::Right => {
				self.switch_tab(1);
				self.rebuild();
				PanelEvent::Consumed
			},
			Key::BackTab | Key::Left => {
				self.switch_tab(-1);
				self.rebuild();
				PanelEvent::Consumed
			},
			Key::Up => {
				if self.move_selection(-1) {
					self.sync_tab_to_selection();
					self.rebuild();
				}
				PanelEvent::Consumed
			},
			Key::Down => {
				if self.move_selection(1) {
					self.sync_tab_to_selection();
					self.rebuild();
				}
				PanelEvent::Consumed
			},
			Key::PageUp => {
				if self.move_selection(-(self.list_rows as isize)) {
					self.sync_tab_to_selection();
					self.rebuild();
				}
				PanelEvent::Consumed
			},
			Key::PageDown => {
				if self.move_selection(self.list_rows as isize) {
					self.sync_tab_to_selection();
					self.rebuild();
				}
				PanelEvent::Consumed
			},
			Key::Home => {
				if self.move_selection(-(self.items.len() as isize)) {
					self.sync_tab_to_selection();
					self.rebuild();
				}
				PanelEvent::Consumed
			},
			Key::End => {
				if self.move_selection(self.items.len() as isize) {
					self.sync_tab_to_selection();
					self.rebuild();
				}
				PanelEvent::Consumed
			},
			Key::Enter => self.activate(),
			Key::Space
				if matches!(
					self.selected().map(|row| &row.widget),
					Some(RowWidget::Boolean | RowWidget::Enum(_))
				) =>
			{
				self.activate()
			},
			Key::Space => {
				self.query.push(' ');
				self.reflow_items();
				self.rebuild();
				PanelEvent::Consumed
			},
			Key::Backspace => {
				if self.query.pop().is_some() {
					self.reflow_items();
					self.rebuild();
				}
				PanelEvent::Consumed
			},
			Key::Char(character) if !character.is_control() => {
				self.query.push(character);
				self.reflow_items();
				self.rebuild();
				PanelEvent::Consumed
			},
			_ => PanelEvent::Consumed,
		}
	}

	fn paste(&mut self, text: &str) -> PanelEvent {
		let clean = text.replace(['\n', '\r', '\t'], " ");
		if let Some(Editor::Text(buffer)) = self.editor.as_mut() {
			buffer.push_str(&clean);
		} else if self.editor.is_none() {
			self.query.push_str(clean.trim());
			self.reflow_items();
		}
		self.rebuild();
		PanelEvent::Consumed
	}

	fn mouse(&mut self, report: MouseReport) -> PanelEvent {
		let event = self
			.ui
			.handle_mouse_with_mods(report.col, report.row, report.kind, report.mods);
		self.sync_pointer_tab();
		match event {
			UiEvent::Cancel => PanelEvent::Close,
			_ => PanelEvent::Consumed,
		}
	}

	fn frame(&mut self, viewport: Size) -> &Frame {
		if viewport.width != self.width || viewport.height != self.height {
			self.width = viewport.width;
			self.height = viewport.height;
			self.rebuild();
		}
		self.ui.frame()
	}
}

#[cfg(test)]
mod tests {
	use omp_con::{DynamicUiSpec, DynamicUiWidget, DynamicVarSpec, SettingTab, TypeSpec, VarFlags};
	use omp_tui::frame_text;

	use super::*;

	fn choice(value: &'static str, label: &'static str) -> Choice {
		Choice {
			value:       Str::new_static(value),
			label:       Str::new_static(label),
			description: Str::default(),
		}
	}

	fn row(
		label: &'static str,
		tab: SettingTab,
		group: &'static str,
		widget: RowWidget,
		value: RowValue,
	) -> SettingRow {
		SettingRow {
			convar: sf!("internal_{}", label.to_ascii_lowercase().replace(' ', "_")),
			pi_path: None,
			label: Str::new_static(label),
			description: sf!("Human description for {label}"),
			warning: None,
			tab,
			group: Str::new_static(group),
			widget,
			value: value.clone(),
			default: value,
			value_kind: ValueKind::Str,
			codec: UiValueCodec::Identity,
			condition: None,
			condition_met: true,
		}
	}

	fn fixture() -> Vec<SettingRow> {
		vec![
			row(
				"Thinking Level",
				SettingTab::Model,
				"Thinking",
				RowWidget::Submenu(vec![choice("low", "Low"), choice("high", "High")]),
				RowValue::Scalar(Str::new_static("low")),
			),
			row(
				"Show Details",
				SettingTab::Appearance,
				"Display",
				RowWidget::Boolean,
				RowValue::Boolean(true),
			),
			row(
				"Profile Name",
				SettingTab::Interaction,
				"Input",
				RowWidget::Text { secret: false },
				RowValue::Text(Str::new_static("Ada")),
			),
			row(
				"Search Engines",
				SettingTab::Providers,
				"Services",
				RowWidget::MultiSelect {
					options: vec![choice("a", "Alpha"), choice("b", "Beta")],
					ordered: true,
				},
				RowValue::Multi(vec![Str::new_static("a")]),
			),
		]
	}

	fn text(panel: &mut SettingsPanel) -> String {
		frame_text(panel.frame(Size { width: 140, height: 32 }))
	}

	#[test]
	fn all_ten_pi_tabs_are_present_and_rows_use_human_labels() {
		let mut panel = SettingsPanel::from_rows(fixture(), &UiContext::default());
		assert_eq!(SETTING_TABS.len(), 10);
		let screen = text(&mut panel);
		for label in [
			"Appearance",
			"Model",
			"Interaction",
			"Context",
			"Memory",
			"Files",
			"Shell",
			"Tools",
			"Tasks",
			"Providers",
		] {
			assert!(screen.contains(label), "missing tab {label}:\n{screen}");
		}
		assert!(screen.contains("Show Details"), "{screen}");
		assert!(!screen.contains("internal_show_details"), "{screen}");
	}

	#[test]
	fn search_uses_human_label_and_description_not_internal_name() {
		let mut panel = SettingsPanel::from_rows(fixture(), &UiContext::default());
		for character in "thinking".chars() {
			panel.key(Key::Char(character));
		}
		assert_eq!(panel.selected().map(|row| row.label.as_str()), Some("Thinking Level"));
		assert!(text(&mut panel).contains("Thinking Level"));
		panel.query.clear();
		panel.reflow_items();
		for character in "internal_profile".chars() {
			panel.key(Key::Char(character));
		}
		assert!(panel.selected().is_none());
	}

	#[test]
	fn archive_without_ui_is_absent_and_dynamic_ui_is_opt_in() {
		let ctx = Ctx::new();
		assert!(
			omp_con::AI_FASTMODE
				.spec()
				.flags
				.contains(VarFlags::ARCHIVE)
		);
		assert!(
			settings_rows(&ctx)
				.iter()
				.all(|row| row.convar != "ai_fastmode")
		);
		ctx.register_dynamic_var(DynamicVarSpec {
			name:    Str::new_static("ext::demo::hidden"),
			desc:    Str::new_static("hidden"),
			ty:      TypeSpec::BOOL,
			flags:   VarFlags::ARCHIVE,
			default: Value::Bool(false),
			ui:      None,
		})
		.unwrap();
		ctx.register_dynamic_var(DynamicVarSpec {
			name:    Str::new_static("ext::demo::visible"),
			desc:    Str::new_static("visible"),
			ty:      TypeSpec::BOOL,
			flags:   VarFlags::ARCHIVE,
			default: Value::Bool(false),
			ui:      Some(DynamicUiSpec {
				tab:         SettingTab::Tools,
				group:       Str::new_static("Extensions"),
				label:       Str::new_static("Demo Extension"),
				description: Str::new_static("Enable the demo extension"),
				warning:     None,
				widget:      DynamicUiWidget::Auto,
			}),
		})
		.unwrap();
		let rows = settings_rows(&ctx);
		assert!(rows.iter().all(|row| row.convar != "ext::demo::hidden"));
		assert!(rows.iter().any(|row| row.label == "Demo Extension"));
	}

	#[test]
	fn bool_enum_submenu_text_and_multiselect_emit_typed_commands() {
		let mut rows = fixture();
		rows.push(row(
			"Mode",
			SettingTab::Appearance,
			"Display",
			RowWidget::Enum(vec![Str::new_static("one"), Str::new_static("two")]),
			RowValue::Scalar(Str::new_static("one")),
		));
		let mut panel = SettingsPanel::from_rows(rows, &UiContext::default());
		assert!(matches!(panel.key(Key::Enter), PanelEvent::Run(_)));
		panel.key(Key::Right);
		assert_eq!(panel.selected().map(|row| row.label.as_str()), Some("Thinking Level"));
		assert_eq!(panel.key(Key::Enter), PanelEvent::Consumed);
		panel.key(Key::Down);
		assert!(
			matches!(&panel.key(Key::Enter), PanelEvent::Run(line) if line.contains(" high; writecfg"))
		);
		panel.key(Key::Right);
		assert_eq!(panel.selected().map(|row| row.label.as_str()), Some("Profile Name"));
		panel.key(Key::Enter);
		panel.key(Key::Ctrl('u'));
		panel.key(Key::Char('B'));
		panel.key(Key::Space);
		panel.key(Key::Char('C'));
		assert!(
			matches!(&panel.key(Key::Enter), PanelEvent::Run(line) if line.contains(" \"B C\"; writecfg"))
		);
		while panel.tab() != SettingTab::Providers {
			panel.key(Key::Right);
		}
		panel.key(Key::Enter);
		panel.key(Key::Down);
		panel.key(Key::Space);
		assert!(
			matches!(&panel.key(Key::Enter), PanelEvent::Run(line) if line.contains("[a b]; writecfg"))
		);
	}

	#[test]
	fn pi_value_codecs_write_underlying_convar_types() {
		let mut setting = row(
			"Converted",
			SettingTab::Appearance,
			"Display",
			RowWidget::Submenu(vec![choice("50", "50 KB")]),
			RowValue::Scalar(Str::new_static("50")),
		);
		setting.codec = UiValueCodec::Kibibytes;
		assert_eq!(SettingsPanel::command_value(&setting, &setting.value).unwrap(), "51200");
		setting.codec = UiValueCodec::PercentFraction;
		assert_eq!(
			SettingsPanel::command_value(&setting, &RowValue::Scalar(Str::new_static("80")),).unwrap(),
			"0.8"
		);
		setting.codec = UiValueCodec::SecondsDuration;
		assert_eq!(
			SettingsPanel::command_value(&setting, &RowValue::Scalar(Str::new_static("30")),).unwrap(),
			"30s"
		);
		setting.codec = UiValueCodec::MillisecondsDuration;
		assert_eq!(
			SettingsPanel::command_value(&setting, &RowValue::Scalar(Str::new_static("300000")),)
				.unwrap(),
			"300000ms"
		);
		setting.widget = RowWidget::Boolean;
		setting.codec = UiValueCodec::InvertedBoolean;
		assert_eq!(
			SettingsPanel::command_value(&setting, &RowValue::Boolean(true)).unwrap(),
			"false"
		);
		setting.codec = UiValueCodec::IsolationEnabled;
		assert_eq!(SettingsPanel::command_value(&setting, &RowValue::Boolean(true)).unwrap(), "auto");
	}

	#[test]
	fn conditions_follow_live_human_controls() {
		let mut driver = row(
			"Usage-Aware Fallback",
			SettingTab::Appearance,
			"Display",
			RowWidget::Boolean,
			RowValue::Boolean(false),
		);
		driver.convar = Str::new_static("ai_retry_usage_aware_fallback");
		let widgets = [
			("Conditional Bool", RowWidget::Boolean, RowValue::Boolean(false)),
			(
				"Conditional Enum",
				RowWidget::Enum(vec![Str::new_static("a")]),
				RowValue::Scalar(Str::new_static("a")),
			),
			(
				"Conditional Submenu",
				RowWidget::Submenu(vec![choice("a", "Alpha")]),
				RowValue::Scalar(Str::new_static("a")),
			),
			(
				"Conditional Text",
				RowWidget::Text { secret: false },
				RowValue::Text(Str::new_static("text")),
			),
			(
				"Conditional Multiselect",
				RowWidget::MultiSelect { options: vec![choice("a", "Alpha")], ordered: false },
				RowValue::Multi(vec![Str::new_static("a")]),
			),
		];
		let mut rows = vec![driver];
		rows.extend(widgets.into_iter().map(|(label, widget, value)| {
			let mut dependent = row(label, SettingTab::Appearance, "Display", widget, value);
			dependent.condition = Some(UiCondition::UsageAwareFallbackEnabled);
			dependent.condition_met = false;
			dependent
		}));
		let mut panel = SettingsPanel::from_rows(rows, &UiContext::default());
		assert_eq!(
			panel
				.items
				.iter()
				.filter(|item| matches!(item, Item::Row(_)))
				.count(),
			1
		);
		panel.key(Key::Enter);
		assert_eq!(
			panel
				.items
				.iter()
				.filter(|item| matches!(item, Item::Row(_)))
				.count(),
			6
		);
	}
}
