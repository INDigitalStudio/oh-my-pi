import { beforeAll, describe, expect, it, mock } from "bun:test";
mock.module("@oh-my-pi/pi-natives", () => ({
	detectMacOSAppearance: () => undefined,
	MacAppearanceObserver: class {
		static start() {
			return { stop() {} };
		}
	},
	HighlightStream: class {},
	highlightCode: (code: string) => code,
	supportsLanguage: () => false,
	warmHighlighter: () => {},
	diffWords: () => [],
	fuzzyFind: () => [],
	matchesKey: () => false,
	parseKey: () => undefined,
	parseKittySequence: () => undefined,
	encodeSixel: () => "",
	FileLock: class {},
	Process: class {},
	ProcessStatus: { Running: 0, Exited: 1 },
	TtyWriter: class {},
	Ellipsis: { Start: 0, Middle: 1, End: 2 },
	extractSegments: () => [],
	setHangulCompatJamoWidthOverride: () => {},
	sliceWithWidth: (text: string) => ({ text, width: text.length }),
	truncateToWidth: (text: string, width: number) => text.slice(0, width),
	wrapTextWithAnsi: (text: string) => [text],
}));

import type { Component, RenderScheduler } from "@oh-my-pi/pi-tui";
import type { ExtensionUiComponentFactory } from "../src/extensibility/extensions/types";

const { visibleWidth } = await import("@oh-my-pi/pi-tui");
const { VirtualTerminal } = await import("../../tui/test/virtual-terminal");
const { withoutTerminalMultiplexer } = await import("./helpers/terminal-multiplexer");
const { TranscriptContainer } = await import("../src/modes/components/transcript-container");
const { COMPOSER_DEFAULTS, Composer } = await import("../src/modes/composer");
const { initTheme } = await import("../src/modes/theme/theme");

withoutTerminalMultiplexer();

class ImmediateScheduler implements RenderScheduler {
	#now = 0;
	now() {
		return this.#now;
	}
	scheduleImmediate(cb: () => void) {
		cb();
		return { cancel() {} };
	}
	scheduleRender(cb: () => void, _ms: number) {
		this.#now += 120;
		cb();
		return { cancel() {} };
	}
}

/** Component that renders N padded rows of a marker. */
class FixedRows implements Component {
	constructor(
		private marker: string,
		private lineCount: number,
	) {}
	invalidate(): void {}
	render(width: number): readonly string[] {
		return Array.from({ length: this.lineCount }, () => this.marker.padEnd(width));
	}
}

/** Pane factory that renders static rows. */
function paneFactory(marker: string, lines: number): ExtensionUiComponentFactory {
	return () => ({
		invalidate() {},
		render(width: number) {
			return Array.from({ length: lines }, () => marker.padEnd(width));
		},
	});
}

function makeComposer(columns = 80, rows = 12): InstanceType<typeof Composer> {
	const terminal = new VirtualTerminal(columns, rows);
	const composer = new Composer({
		terminal,
		tuiOptions: { renderScheduler: new ImmediateScheduler() },
		preferences: { ...COMPOSER_DEFAULTS, quiet: true },
	});
	composer.setRuntimeChildren([new TranscriptContainer(), new FixedRows("MAIN", 3)]);
	composer.start({ playWelcomeIntro: false });
	return composer;
}

/** renderFrame shorthand returning viewport rows. */
function frame(composer: InstanceType<typeof Composer>, columns: number, rows: number) {
	return [...composer.renderFrame({ columns, rows }).viewport];
}

/** Strip ANSI from a string. */
const strip = (s: string) => Bun.stripANSI(s);

describe("Composer side pane", () => {
	beforeAll(async () => {
		await initTheme();
	});

	it("splits viewport into main + pane when pane is set", () => {
		const composer = makeComposer(100, 10);
		composer.setSidePane("test", paneFactory("PANE", 5), { width: 25 });
		const vp = frame(composer, 100, 10);
		expect(vp.length).toBe(10);
		const withPane = vp.filter(row => strip(row).includes("PANE"));
		expect(withPane.length).toBe(5);
	});

	it("hides pane when terminal is too narrow", () => {
		const composer = makeComposer(79, 10);
		// minMainWidth=60, minWidth=20, +1 divider → need ≥81 columns; 79 is too narrow
		composer.setSidePane("test", paneFactory("PANE", 5), { width: 20, minWidth: 20, minMainWidth: 60 });
		const vp = frame(composer, 79, 10);
		expect(vp.every(row => !strip(row).includes("PANE"))).toBe(true);
	});

	it("removes pane on setSidePane(key, undefined)", () => {
		const composer = makeComposer(100, 10);
		composer.setSidePane("test", paneFactory("PANE", 5), { width: 25 });
		expect(frame(composer, 100, 10).some(r => strip(r).includes("PANE"))).toBe(true);

		composer.setSidePane("test", undefined);
		expect(frame(composer, 100, 10).every(r => !strip(r).includes("PANE"))).toBe(true);
	});

	it("replaces pane when a different key is set", () => {
		const composer = makeComposer(100, 10);
		composer.setSidePane("k1", paneFactory("OLD", 5), { width: 25 });
		expect(frame(composer, 100, 10).some(r => strip(r).includes("OLD"))).toBe(true);

		composer.setSidePane("k2", paneFactory("NEW", 5), { width: 25 });
		const vp = frame(composer, 100, 10);
		expect(vp.every(r => !strip(r).includes("OLD"))).toBe(true);
		expect(vp.some(r => strip(r).includes("NEW"))).toBe(true);
	});

	it("updates options without replacing component for same key", () => {
		const composer = makeComposer(100, 10);
		let creations = 0;
		const factory = paneFactory("PANE", 5);
		composer.setSidePane(
			"k",
			(...args) => {
				creations++;
				return factory(...args);
			},
			{ width: 25 },
		);
		frame(composer, 100, 10);
		composer.setSidePane(
			"k",
			() => {
				throw new Error("same-key update replaced the component");
			},
			{ width: 20 },
		);
		expect(frame(composer, 100, 10).some(r => strip(r).includes("PANE"))).toBe(true);
		expect(creations).toBe(1);
	});

	it("disposes the component when the pane is cleared", () => {
		const composer = makeComposer(100, 10);
		let disposed = false;
		composer.setSidePane(
			"k",
			() => ({
				render: width => ["PANE".padEnd(width)],
				dispose: () => {
					disposed = true;
				},
			}),
			{ width: 20 },
		);
		frame(composer, 100, 10);
		composer.clearSidePane();
		expect(disposed).toBe(true);
	});

	it("pane rows are top-aligned within viewport", () => {
		const composer = makeComposer(100, 10);
		composer.setSidePane("test", paneFactory("TOP", 3), { width: 25 });
		const vp = frame(composer, 100, 10);
		const topInFirst3 = vp.slice(0, 3).filter(r => strip(r).includes("TOP")).length;
		const topInLast3 = vp.slice(-3).filter(r => strip(r).includes("TOP")).length;
		expect(topInFirst3).toBe(3);
		expect(topInLast3).toBe(0);
	});

	it("pane content never enters history", () => {
		const composer = makeComposer(100, 10);
		composer.setSidePane("test", paneFactory("PANE", 5), { width: 25 });
		const plan = composer.renderFrame({ columns: 100, rows: 10 });
		if (plan.history) {
			expect(plan.history.rows.every(row => !strip(row).includes("PANE"))).toBe(true);
		}
	});

	it("each composed row spans the full terminal width", () => {
		const composer = makeComposer(100, 10);
		composer.setSidePane("test", paneFactory("P", 5), { width: 25 });
		const vp = frame(composer, 100, 10);
		for (const row of vp) {
			expect(visibleWidth(row)).toBe(100);
		}
	});
});
