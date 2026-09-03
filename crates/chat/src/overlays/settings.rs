//! `/settings`: pi's settings selector (`settings-selector.ts`,
//! `settings-defs.ts`) over the console variable registry (ADR 0012).
//!
//! pi generates its widget tree from the settings schema; on this host the
//! schema *is* the `omp_con` registry: one row per `ARCHIVE` variable, typed
//! by the variable's [`TypeSpec`] (bool toggle, enum cycle, number and
//! duration editors, string editor, list editor), grouped into pi's tabs by
//! prefix (`ai_` Model, `cl_` Interface, `sv_` Server), described by the
//! variable's doc comment. Every change applies live through the one
//! command stream (`<name> <value>; writecfg`, ADR 0014) so `config.cfg`
//! carries it and the running actor sees it at once.

use omp_con::{Ctx, RegItem, Span, TypeSpec, Value, ValueKind, VarFlags};
use omp_core::{Str, sf};
use omp_tui::{
	Component, Frame, IntoComponent as _, Key, MouseReport, Prop, Size, Ui, UiContext, UiEvent,
	cell_width, components::Tabs, dom,
};

use super::{Panel, PanelAnchor, PanelCx, PanelEvent};

/// pi settings footer.
const FOOTER: &str = "↑/↓ navigate · ←/→ tab · Enter change · type to search · Esc close";
const EDIT_FOOTER: &str = "Enter apply · Esc cancel · Ctrl+U clear";
/// Border rows, tab bar and its rule, search row, the divider, the pinned
/// description, and the footer.
const CHROME_ROWS: u16 = 9;
/// Empty-string display.
const EMPTY: &str = "(empty)";

/// pi settings tab (`SETTING_TABS`) a variable prefix maps to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::Display)]
pub enum Group {
	/// `ai_*`: model, thinking, compaction, fast mode.
	Model,
	/// `cl_*`: transcript, composer, theme, speech.
	Interface,
	/// `sv_*`: tools, approval, environment.
	Server,
	/// Anything else (extension-declared knobs).
	Other,
}

impl Group {
	/// The tab a variable name belongs to.
	#[must_use]
	pub fn of(name: &str) -> Self {
		match name.split_once('_').map(|(prefix, _)| prefix) {
			Some("ai") => Self::Model,
			Some("cl") => Self::Interface,
			Some("sv") => Self::Server,
			_ => Self::Other,
		}
	}

	const fn icon(self) -> &'static str {
		match self {
			Self::Model => "model",
			Self::Interface => "appearance",
			Self::Server => "gear",
			Self::Other => "config",
		}
	}
}

/// Widget a row edits with (pi `SettingDef.type`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Widget {
	/// Enter/Space flips (pi `boolean`).
	Bool,
	/// Enter cycles the declared variants (pi `enum`).
	Enum,
	/// Inline editor (pi `text` with a number pattern).
	Int,
	/// Inline editor.
	Float,
	/// Inline editor over a span literal (`90s`, `never`).
	Duration,
	/// Inline editor (pi `text`).
	Text,
	/// Inline editor over whitespace-separated items (pi `multiselect`).
	List,
	/// Inline editor over the key/value block literal.
	Kv,
}

impl Widget {
	const fn of(spec: &TypeSpec) -> Self {
		match spec.kind {
			ValueKind::Bool => Self::Bool,
			ValueKind::Enum => Self::Enum,
			ValueKind::Int => Self::Int,
			ValueKind::Float => Self::Float,
			ValueKind::Duration => Self::Duration,
			ValueKind::Str => Self::Text,
			ValueKind::List => Self::List,
			ValueKind::Kv => Self::Kv,
		}
	}
}

/// One editable variable.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingRow {
	/// Console name (`ai_fastmode`).
	pub name:     Str,
	/// Doc comment of the declaration.
	pub desc:     Str,
	/// Tab.
	pub group:    Group,
	/// Editing widget.
	pub widget:   Widget,
	/// Enum variants (empty unless `widget == Enum`).
	pub variants: &'static [&'static str],
	/// List element kind (`Str` unless declared).
	pub elem:     ValueKind,
	/// Live value.
	pub value:    Value,
	/// Registration default.
	pub default:  Value,
	/// Numeric clamps.
	pub min:      Option<f64>,
	/// Numeric clamps.
	pub max:      Option<f64>,
}

impl SettingRow {
	/// Whether the value diverges from its default (pi `changed`).
	#[must_use]
	pub fn changed(&self) -> bool {
		self.value != self.default
	}

	/// The value as pi's row shows it: unquoted, lists comma-joined.
	#[must_use]
	pub fn display(&self) -> Str {
		display_value(&self.value)
	}

	/// The value as an editor seed (lists space-joined, strings raw).
	fn editable(&self) -> String {
		match &self.value {
			Value::Str(text) => text.to_string(),
			Value::List(items) => items
				.iter()
				.map(|item| display_value(item).to_string())
				.collect::<Vec<_>>()
				.join(" "),
			other => other.to_string(),
		}
	}
}

fn display_value(value: &Value) -> Str {
	match value {
		Value::Str(text) | Value::Enum(text) if text.is_empty() => Str::new_static(EMPTY),
		Value::Str(text) | Value::Enum(text) => text.clone(),
		Value::List(items) if items.is_empty() => Str::new_static(EMPTY),
		Value::List(items) => Str::new(
			items
				.iter()
				.map(|item| display_value(item).to_string())
				.collect::<Vec<_>>()
				.join(", "),
		),
		other => Str::new(other.to_string()),
	}
}

/// Every `ARCHIVE` variable of `con` as a settings row, in name order.
#[must_use]
pub fn archive_rows(con: &Ctx) -> Vec<SettingRow> {
	let mut rows = con
		.items()
		.filter_map(|item| match item {
			RegItem::Var(spec) if spec.flags.contains(VarFlags::ARCHIVE) => Some(spec),
			_ => None,
		})
		.map(|spec| SettingRow {
			name:     Str::new_static(spec.name),
			desc:     Str::new(spec.desc.trim()),
			group:    Group::of(spec.name),
			widget:   Widget::of(spec.ty),
			variants: spec.ty.variants,
			elem:     spec.ty.elem.map_or(ValueKind::Str, |elem| elem.kind),
			value:    con.get(spec.name).unwrap_or_else(|| (spec.default)()),
			default:  (spec.default)(),
			min:      spec.min,
			max:      spec.max,
		})
		.collect::<Vec<_>>();
	rows.sort_by(|a, b| a.name.cmp(&b.name));
	rows
}

/// One flattened list entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Item {
	/// Group heading (search results only).
	Header(Group),
	/// Index into the panel's rows.
	Row(usize),
}

/// Retained settings selector.
pub struct SettingsPanel {
	rows:      Vec<SettingRow>,
	tabs:      Vec<Group>,
	tab:       usize,
	query:     String,
	items:     Vec<Item>,
	selected:  usize,
	scroll:    usize,
	list_rows: usize,
	editor:    Option<String>,
	ui:        Ui,
	ctx:       UiContext,
	width:     u16,
	height:    u16,
}

impl SettingsPanel {
	/// Opens the selector over the console registry.
	pub fn open(cx: &PanelCx<'_>) -> Result<Self, Str> {
		let rows = archive_rows(cx.con);
		if rows.is_empty() {
			return Err(Str::new_static("No archived settings are registered"));
		}
		Ok(Self::from_rows(rows, cx.ui))
	}

	/// Builds the selector over explicit rows (tests, fixtures).
	#[must_use]
	pub fn from_rows(mut rows: Vec<SettingRow>, ctx: &UiContext) -> Self {
		rows.sort_by(|a, b| a.name.cmp(&b.name));
		let tabs = [Group::Model, Group::Interface, Group::Server, Group::Other]
			.into_iter()
			.filter(|group| rows.iter().any(|row| row.group == *group))
			.collect::<Vec<_>>();
		let mut panel = Self {
			rows,
			tabs,
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

	/// Active tab.
	#[must_use]
	pub fn tab(&self) -> Group {
		self.tabs.get(self.tab).copied().unwrap_or(Group::Other)
	}

	/// Row under the cursor.
	#[must_use]
	pub fn selected(&self) -> Option<&SettingRow> {
		match self.items.get(self.selected)? {
			Item::Row(index) => self.rows.get(*index),
			Item::Header(_) => None,
		}
	}

	/// Live search query.
	#[must_use]
	pub fn query(&self) -> &str {
		&self.query
	}

	/// Whether the inline editor is open.
	#[must_use]
	pub const fn editing(&self) -> bool {
		self.editor.is_some()
	}

	/// Rebuilds the flattened list for the tab or the search query and
	/// parks the cursor on the first row (pi resets on tab and query
	/// changes).
	fn reflow_items(&mut self) {
		self.items.clear();
		if self.query.is_empty() {
			let group = self.tab();
			self.items.extend(
				self
					.rows
					.iter()
					.enumerate()
					.filter(|(_, row)| row.group == group)
					.map(|(index, _)| Item::Row(index)),
			);
		} else {
			let query = self.query.to_ascii_lowercase();
			for group in &self.tabs {
				let mut first = true;
				for (index, row) in self.rows.iter().enumerate() {
					if row.group != *group {
						continue;
					}
					if !(row.name.contains(query.as_str())
						|| row.desc.to_ascii_lowercase().contains(query.as_str()))
					{
						continue;
					}
					if first {
						self.items.push(Item::Header(*group));
						first = false;
					}
					self.items.push(Item::Row(index));
				}
			}
		}
		self.selected = self
			.items
			.iter()
			.position(|item| matches!(item, Item::Row(_)))
			.unwrap_or(0);
		self.scroll = 0;
		self.clamp_scroll();
		if !self.query.is_empty() {
			self.sync_tab_to_selection();
		}
	}

	fn clamp_scroll(&mut self) {
		if self.selected < self.scroll {
			self.scroll = self.selected;
		} else if self.selected >= self.scroll + self.list_rows {
			self.scroll = self.selected + 1 - self.list_rows;
		}
		let max = self.items.len().saturating_sub(self.list_rows);
		self.scroll = self.scroll.min(max);
	}

	/// Moves the cursor by `delta` rows, skipping headings; `false` at the
	/// edges.
	fn move_selection(&mut self, delta: isize) -> bool {
		if self.items.is_empty() || delta == 0 {
			return false;
		}
		let mut next = self.selected;
		let mut last_row = self.selected;
		let mut moved = 0;
		while moved < delta.unsigned_abs() {
			let Some(candidate) = next.checked_add_signed(delta.signum()) else { break };
			if candidate >= self.items.len() {
				break;
			}
			next = candidate;
			if matches!(self.items[next], Item::Row(_)) {
				last_row = next;
				moved += 1;
			}
		}
		if last_row == self.selected {
			return false;
		}
		let next = last_row;
		self.selected = next;
		self.clamp_scroll();
		true
	}

	fn switch_tab(&mut self, delta: isize) {
		if self.tabs.is_empty() {
			return;
		}
		let len = self.tabs.len() as isize;
		if self.query.is_empty() {
			self.tab = ((self.tab as isize + delta).rem_euclid(len)) as usize;
			self.reflow_items();
			return;
		}
		// Search mode: jump to the next group heading that has matches.
		let headers = self
			.items
			.iter()
			.enumerate()
			.filter(|(_, item)| matches!(item, Item::Header(_)))
			.map(|(index, _)| index)
			.collect::<Vec<_>>();
		if headers.is_empty() {
			return;
		}
		let current = headers
			.iter()
			.rposition(|header| *header <= self.selected)
			.unwrap_or(0);
		let next = ((current as isize + delta).rem_euclid(headers.len() as isize)) as usize;
		self.selected = headers[next] + 1;
		self.sync_tab_to_selection();
		self.clamp_scroll();
	}

	/// Keeps the tab chip on the group owning the selected result (pi
	/// `#syncTabBarToSelection`).
	fn sync_tab_to_selection(&mut self) {
		if let Some(row) = self.selected()
			&& let Some(tab) = self.tabs.iter().position(|group| *group == row.group)
		{
			self.tab = tab;
		}
	}

	fn select_tab(&mut self, tab: usize) {
		if tab >= self.tabs.len() || tab == self.tab {
			return;
		}
		if self.query.is_empty() {
			self.tab = tab;
			self.reflow_items();
		} else {
			let group = self.tabs[tab];
			if let Some(header) = self
				.items
				.iter()
				.position(|item| matches!(item, Item::Header(candidate) if *candidate == group))
			{
				self.tab = tab;
				self.selected = header + 1;
				self.clamp_scroll();
			}
		}
		self.rebuild();
	}

	fn sync_pointer_tab(&mut self) {
		let values = self.ui.values();
		let Some(label) = values.get("groups").and_then(|value| value.as_str()) else {
			return;
		};
		let name = label.split_once(" (").map_or(label, |(name, _)| name);
		let group = match name {
			"Model" => Group::Model,
			"Interface" => Group::Interface,
			"Server" => Group::Server,
			"Other" => Group::Other,
			_ => return,
		};
		if let Some(tab) = self.tabs.iter().position(|candidate| *candidate == group) {
			self.select_tab(tab);
		}
	}

	fn end_search(&mut self) {
		let keep = self.selected().map(|row| row.name.clone());
		self.sync_tab_to_selection();
		self.query.clear();
		self.reflow_items();
		if let Some(name) = keep
			&& let Some(index) = self
				.items
				.iter()
				.position(|item| matches!(item, Item::Row(i) if self.rows[*i].name == name))
		{
			self.selected = index;
			self.clamp_scroll();
		}
	}

	/// pi `SettingsList` activation: booleans flip, enums cycle, everything
	/// else opens the inline editor.
	fn activate(&mut self) -> PanelEvent {
		let Some(Item::Row(index)) = self.items.get(self.selected).copied() else {
			return PanelEvent::Consumed;
		};
		let row = &self.rows[index];
		match row.widget {
			Widget::Bool => {
				let next = Value::Bool(!row.value.as_bool().unwrap_or(false));
				self.commit(index, next)
			},
			Widget::Enum => {
				let variants = row.variants;
				if variants.is_empty() {
					return PanelEvent::Consumed;
				}
				let at = row
					.value
					.as_str()
					.and_then(|current| variants.iter().position(|variant| *variant == current))
					.map_or(0, |at| (at + 1) % variants.len());
				self.commit(index, Value::Enum(Str::new_static(variants[at])))
			},
			_ => {
				self.editor = Some(row.editable());
				self.rebuild();
				PanelEvent::Consumed
			},
		}
	}

	/// Parses the editor buffer for the selected row's widget.
	fn parse_editor(&self, text: &str) -> Result<Value, Str> {
		let Some(row) = self.selected() else {
			return Err(Str::new_static("No setting selected"));
		};
		let text = text.trim();
		let number = |value: f64| -> Result<f64, Str> {
			if let Some(min) = row.min
				&& value < min
			{
				return Err(sf!("{} must be at least {min}", row.name));
			}
			if let Some(max) = row.max
				&& value > max
			{
				return Err(sf!("{} must be at most {max}", row.name));
			}
			Ok(value)
		};
		Ok(match row.widget {
			Widget::Int => Value::Int(
				text
					.parse::<i64>()
					.map_err(|_| sf!("{} expects an integer, got {text:?}", row.name))
					.and_then(|value| number(value as f64).map(|_| value))?,
			),
			Widget::Float => Value::Float(
				text
					.parse::<f64>()
					.map_err(|_| sf!("{} expects a number, got {text:?}", row.name))
					.and_then(number)?,
			),
			Widget::Duration => Value::Duration(
				text
					.parse::<Span>()
					.map_err(|_| sf!("{} expects a duration such as 90s or never, got {text:?}", row.name))?,
			),
			Widget::Text => Value::Str(Str::new(text)),
			Widget::List => Value::List(
				text
					.split_whitespace()
					.map(|item| match row.elem {
						ValueKind::Int => item
							.parse::<i64>()
							.map(Value::Int)
							.map_err(|_| sf!("{} expects integers, got {item:?}", row.name)),
						ValueKind::Float => item
							.parse::<f64>()
							.map(Value::Float)
							.map_err(|_| sf!("{} expects numbers, got {item:?}", row.name)),
						ValueKind::Bool => match item {
							"true" | "1" => Ok(Value::Bool(true)),
							"false" | "0" => Ok(Value::Bool(false)),
							_ => Err(sf!("{} expects booleans, got {item:?}", row.name)),
						},
						_ => Ok(Value::Str(Str::new(item))),
					})
					.collect::<Result<Vec<_>, _>>()?,
			),
			Widget::Kv | Widget::Bool | Widget::Enum => {
				return Err(sf!("{} cannot be edited inline", row.name));
			},
		})
	}

	/// Applies `value` to the row locally and asks the host to run the
	/// same write through the console, archiving it (ADR 0014: one command
	/// stream for keys, commands, and scripts).
	fn commit(&mut self, index: usize, value: Value) -> PanelEvent {
		let row = &mut self.rows[index];
		if row.value == value {
			self.rebuild();
			return PanelEvent::Consumed;
		}
		row.value = value;
		let line = sf!("{} {}; writecfg", row.name, row.value);
		self.rebuild();
		PanelEvent::Run(line)
	}

	fn editor_key(&mut self, key: Key) -> PanelEvent {
		let Some(buffer) = self.editor.as_mut() else {
			return PanelEvent::Ignored;
		};
		match key {
			Key::Esc => {
				self.editor = None;
				self.rebuild();
			},
			Key::Enter => {
				let text = std::mem::take(buffer);
				self.editor = None;
				let Some(Item::Row(index)) = self.items.get(self.selected).copied() else {
					self.rebuild();
					return PanelEvent::Consumed;
				};
				let raw = text.trim();
				if self.rows[index].widget == Widget::Kv {
					// The console parses the block literal; a rejected write
					// surfaces as its own notice.
					self.rebuild();
					return PanelEvent::Run(sf!("{} {raw}; writecfg", self.rows[index].name));
				}
				return match self.parse_editor(&text) {
					Ok(value) => self.commit(index, value),
					Err(error) => {
						self.rebuild();
						PanelEvent::Notice(error)
					},
				};
			},
			Key::Backspace => {
				buffer.pop();
				self.rebuild();
			},
			Key::Space => {
				buffer.push(' ');
				self.rebuild();
			},
			Key::Ctrl('u') => {
				buffer.clear();
				self.rebuild();
			},
			Key::Ctrl('w') => {
				let trimmed = buffer.trim_end().len();
				buffer.truncate(trimmed);
				let cut = buffer.rfind(' ').map_or(0, |at| at + 1);
				buffer.truncate(cut);
				self.rebuild();
			},
			Key::Char(character) if !character.is_control() => {
				buffer.push(character);
				self.rebuild();
			},
			_ => {},
		}
		PanelEvent::Consumed
	}

	fn rebuild(&mut self) {
		let inner = usize::from(self.width.saturating_sub(4).max(20));
		let content_rows = self.height.saturating_sub(CHROME_ROWS).max(3);
		let list_rows = usize::from(content_rows);
		if list_rows != self.list_rows {
			self.list_rows = list_rows;
			self.clamp_scroll();
		}
		let name_width = self
			.rows
			.iter()
			.map(|row| usize::from(cell_width(&row.name)))
			.max()
			.unwrap_or(8)
			.clamp(8, inner.saturating_sub(24).max(8));
		let mut list = self
			.items
			.iter()
			.enumerate()
			.skip(self.scroll)
			.take(self.list_rows)
			.map(|(index, item)| self.list_row(*item, index == self.selected, name_width))
			.collect::<Vec<_>>();
		let empty = self.items.is_empty();
		// A full-screen panel keeps its footer at the bottom: pad the list
		// to the viewport (one row is the empty-state line when it shows).
		let shown = list.len() + usize::from(empty);
		list.extend(
			std::iter::repeat_with(|| dom! { <text>{" "}</text> }.into_component())
				.take(self.list_rows.saturating_sub(shown)),
		);
		let searching = !self.query.is_empty();
		let query = Str::new(self.query.as_str());
		let description = self
			.selected()
			.map(|row| row.desc.clone())
			.unwrap_or_default();
		let footer = Str::new_static(if self.editor.is_some() { EDIT_FOOTER } else { FOOTER });
		let mut tabs = Tabs::new().with_str(Prop::Id, "groups");
		for group in &self.tabs {
			let count = self.rows.iter().filter(|row| row.group == *group).count();
			let title = if searching {
				let matches = self
					.items
					.iter()
					.filter(|item| matches!(item, Item::Row(i) if self.rows[*i].group == *group))
					.count();
				sf!("{group} ({matches})")
			} else {
				sf!("{group} ({count})")
			};
			tabs = tabs.pane_icon(group.icon(), title, dom! { <col/> });
		}
		let tabs = tabs.select(self.tab as u16);
		let tree = dom! {
			<box border=round title="Settings">
				<col>
					{tabs}
					if searching {
						<row gap=1><text fg=muted>{"Search:"}</text><row><text>{query}</text><text fg=accent>{"_"}</text></row></row>
					} else {
						<text fg=muted>{"Type to search all settings"}</text>
					}
					<text>{" "}</text>
					if empty {
						<text fg=muted truncate>{"  No settings match."}</text>
					}
					for row in list { {row} }
					<hr border=round/>
					<text fg=muted truncate>{description}</text>
					<text fg=muted truncate>{footer}</text>
				</col>
			</box>
		};
		self.ui = Ui::from_root(tree, self.width, self.ctx.clone());
	}

	fn list_row(&self, item: Item, selected: bool, name_width: usize) -> Box<dyn Component> {
		match item {
			Item::Header(group) => {
				let icon = group.icon();
				let label = Str::new(group.to_string());
				dom! {
					<row gap=1>
						<icon name={icon} fg=muted/>
						<text bold fg=muted>{label}</text>
					</row>
				}
				.into_component()
			},
			Item::Row(index) => {
				let row = &self.rows[index];
				let marker = if row.changed() { "●" } else { " " };
				let name = pad(&row.name, name_width);
				let value = match (&self.editor, selected) {
					(Some(buffer), true) => sf!("{buffer}_"),
					_ => row.display(),
				};
				let editing = selected && self.editor.is_some();
				if selected {
					dom! {
						<row gap=1 bg=surface>
							<text fg=accent>{marker}</text>
							<pre bold fg=accent>{name}</pre>
							if editing {
								<text fg=warn truncate>{value}</text>
							} else {
								<text fg=accent truncate>{value}</text>
							}
						</row>
					}
					.into_component()
				} else {
					dom! {
						<row gap=1>
							<text fg=accent>{marker}</text>
							<pre>{name}</pre>
							<text fg=muted truncate>{value}</text>
						</row>
					}
					.into_component()
				}
			},
		}
	}
}

/// pi `#padText`: the name column padded to `width` cells.
fn pad(name: &str, width: usize) -> Str {
	let used = usize::from(cell_width(name));
	if used >= width {
		return Str::new(name);
	}
	let mut text = String::with_capacity(width);
	text.push_str(name);
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
			Key::Esc => {
				if self.query.is_empty() {
					return PanelEvent::Close;
				}
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
			Key::Space => {
				if let Some(Widget::Bool | Widget::Enum) = self.selected().map(|row| row.widget) {
					return self.activate();
				}
				self.query.push(' ');
				self.reflow_items();
				self.rebuild();
				PanelEvent::Consumed
			},
			Key::Backspace => {
				if self.query.pop().is_some() {
					if self.query.is_empty() {
						self.end_search();
					} else {
						self.reflow_items();
					}
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
		if let Some(buffer) = self.editor.as_mut() {
			buffer.push_str(&clean);
		} else {
			self.query.push_str(clean.trim());
			self.reflow_items();
		}
		self.rebuild();
		PanelEvent::Consumed
	}

	fn mouse(&mut self, report: MouseReport) -> PanelEvent {
		let event =
			self.ui.handle_mouse_with_mods(report.col, report.row, report.kind, report.mods);
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
	use omp_tui::{Mods, Mouse, MouseButton, frame_text};

	use super::*;

	fn row(name: &'static str, widget: Widget, value: Value, default: Value) -> SettingRow {
		SettingRow {
			name: Str::new_static(name),
			desc: sf!("Doc for {name}."),
			group: Group::of(name),
			widget,
			variants: if widget == Widget::Enum { &["off", "low", "high"] } else { &[] },
			elem: ValueKind::Str,
			value,
			default,
			min: None,
			max: None,
		}
	}

	fn mouse_click(col: u16, row: u16) -> MouseReport {
		MouseReport {
			kind: Mouse::Click,
			col,
			row,
			button: MouseButton::Left,
			mods: Mods::default(),
			pressed: true,
		}
	}

	fn point(text: &str, needle: &str) -> (u16, u16) {
		text.lines()
			.enumerate()
			.find_map(|(row, line)| {
				let byte = line.find(needle)?;
				Some((omp_tui::cell_width(&line[..byte]), u16::try_from(row).unwrap()))
			})
			.expect("text point")
	}

	fn fixture() -> Vec<SettingRow> {
		vec![
			row("ai_fastmode", Widget::Bool, Value::Bool(false), Value::Bool(false)),
			row(
				"ai_thinking",
				Widget::Enum,
				Value::Enum(Str::new_static("low")),
				Value::Enum(Str::new_static("off")),
			),
			row("ai_compact_threshold", Widget::Int, Value::Int(80), Value::Int(80)),
			row(
				"cl_theme",
				Widget::Text,
				Value::Str(Str::new_static("cyanotype")),
				Value::Str(Str::new_static("")),
			),
			row("cl_showthinking", Widget::Bool, Value::Bool(true), Value::Bool(true)),
			row(
				"sv_tools",
				Widget::List,
				Value::List(vec![Value::Str(Str::new_static("read"))]),
				Value::List(Vec::new()),
			),
		]
	}

	fn panel() -> SettingsPanel {
		SettingsPanel::from_rows(fixture(), &UiContext::default())
	}

	fn text(panel: &mut SettingsPanel) -> String {
		frame_text(panel.frame(Size { width: 100, height: 30 }))
	}

	#[test]
	fn rows_group_by_prefix_into_pi_tabs() {
		let mut panel = panel();
		assert_eq!(panel.tabs, [Group::Model, Group::Interface, Group::Server]);
		assert_eq!(panel.selected().map(|row| row.name.as_str()), Some("ai_compact_threshold"));
		let text = text(&mut panel);
		assert!(text.contains("Settings"), "{text}");
		assert!(text.contains("ai_thinking"), "{text}");
		assert!(!text.contains("cl_theme"), "other tabs hidden:\n{text}");
		assert!(text.contains("Doc for ai_compact_threshold."), "pinned description:\n{text}");
	}

	#[test]
	fn enter_flips_a_bool_and_runs_the_archived_write() {
		let mut panel = panel();
		panel.key(Key::Down);
		assert_eq!(
			panel.key(Key::Enter),
			PanelEvent::Run(Str::new_static("ai_fastmode true; writecfg"))
		);
		assert!(panel.selected().unwrap().changed());
		assert_eq!(panel.key(Key::Space), PanelEvent::Run(Str::new_static("ai_fastmode false; writecfg")));
	}

	#[test]
	fn enter_cycles_an_enum_through_its_variants() {
		let mut panel = panel();
		panel.key(Key::Down);
		panel.key(Key::Down);
		assert_eq!(
			panel.key(Key::Enter),
			PanelEvent::Run(Str::new_static("ai_thinking high; writecfg"))
		);
		assert_eq!(
			panel.key(Key::Enter),
			PanelEvent::Run(Str::new_static("ai_thinking off; writecfg"))
		);
	}

	#[test]
	fn enter_opens_the_number_editor_and_applies_the_typed_value() {
		let mut panel = panel();
		assert_eq!(panel.key(Key::Enter), PanelEvent::Consumed);
		assert!(panel.editing());
		panel.key(Key::Ctrl('u'));
		for character in "95".chars() {
			panel.key(Key::Char(character));
		}
		assert_eq!(
			panel.key(Key::Enter),
			PanelEvent::Run(Str::new_static("ai_compact_threshold 95; writecfg"))
		);
		assert!(!panel.editing());
	}

	#[test]
	fn editor_rejects_a_non_number_with_a_notice() {
		let mut panel = panel();
		panel.key(Key::Enter);
		panel.key(Key::Ctrl('u'));
		panel.key(Key::Char('x'));
		assert!(matches!(panel.key(Key::Enter), PanelEvent::Notice(_)));
		assert_eq!(panel.selected().unwrap().value, Value::Int(80));
	}

	#[test]
	fn tabs_switch_with_arrows_and_wrap() {
		let mut panel = panel();
		panel.key(Key::Right);
		assert_eq!(panel.tab(), Group::Interface);
		assert_eq!(panel.selected().map(|row| row.name.as_str()), Some("cl_showthinking"));
		panel.key(Key::Right);
		assert_eq!(panel.tab(), Group::Server);
		panel.key(Key::Tab);
		assert_eq!(panel.tab(), Group::Model);
		panel.key(Key::BackTab);
		assert_eq!(panel.tab(), Group::Server);
	}

	#[test]
	fn clicking_a_tab_reflows_to_that_group() {
		let mut panel = SettingsPanel::from_rows(fixture(), &UiContext::default());
		let size = Size { width: 80, height: 24 };
		let text = frame_text(panel.frame(size));
		let (col, row) = point(&text, "Interface (");
		assert_eq!(panel.mouse(mouse_click(col, row)), PanelEvent::Consumed);
		assert_eq!(panel.tab(), Group::Interface);
		assert_eq!(panel.selected().map(|row| row.group), Some(Group::Interface));
	}

	#[test]
	fn typing_searches_every_tab_and_escape_ends_the_search_first() {
		let mut panel = panel();
		for character in "theme".chars() {
			panel.key(Key::Char(character));
		}
		assert_eq!(panel.query(), "theme");
		assert_eq!(panel.selected().map(|row| row.name.as_str()), Some("cl_theme"));
		assert_eq!(panel.tab(), Group::Interface);
		let text = text(&mut panel);
		assert!(text.contains("Search: theme_"), "{text}");
		assert!(text.contains("Interface"), "{text}");
		assert_eq!(panel.key(Key::Esc), PanelEvent::Consumed);
		assert_eq!(panel.query(), "");
		assert_eq!(panel.tab(), Group::Interface);
		assert_eq!(panel.selected().map(|row| row.name.as_str()), Some("cl_theme"));
		assert_eq!(panel.key(Key::Esc), PanelEvent::Close);
	}

	#[test]
	fn text_and_list_editors_quote_and_bracket_their_values() {
		let mut panel = panel();
		panel.key(Key::Right);
		panel.key(Key::Down);
		assert_eq!(panel.selected().map(|row| row.name.as_str()), Some("cl_theme"));
		panel.key(Key::Enter);
		panel.key(Key::Ctrl('u'));
		for character in "dark mode".chars() {
			if character == ' ' {
				panel.key(Key::Space);
			} else {
				panel.key(Key::Char(character));
			}
		}
		assert_eq!(
			panel.key(Key::Enter),
			PanelEvent::Run(Str::new_static("cl_theme \"dark mode\"; writecfg"))
		);
		panel.key(Key::Right);
		assert_eq!(panel.selected().map(|row| row.name.as_str()), Some("sv_tools"));
		panel.key(Key::Enter);
		panel.key(Key::Space);
		for character in "edit".chars() {
			panel.key(Key::Char(character));
		}
		assert_eq!(
			panel.key(Key::Enter),
			PanelEvent::Run(Str::new_static("sv_tools [read edit]; writecfg"))
		);
	}

	#[test]
	fn archive_rows_come_from_the_registry_with_docs() {
		let con = Ctx::new();
		let rows = archive_rows(&con);
		let fast = rows
			.iter()
			.find(|row| row.name == "ai_fastmode")
			.expect("ai_fastmode is archived");
		assert_eq!(fast.widget, Widget::Bool);
		assert_eq!(fast.group, Group::Model);
		assert!(!fast.desc.is_empty());
		assert!(rows.windows(2).all(|pair| pair[0].name <= pair[1].name));
	}
}
