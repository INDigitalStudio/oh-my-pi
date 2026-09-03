import { afterEach, describe, expect, it } from "bun:test";
import * as path from "node:path";
import { getOrCreateClient, sendRequest, shutdownAll } from "@oh-my-pi/pi-coding-agent/lsp/client";
import type { ServerConfig } from "@oh-my-pi/pi-coding-agent/lsp/types";
import { TempDir } from "@oh-my-pi/pi-utils";

const fixturePath = path.join(import.meta.dir, "..", "fixtures", "fake-lsp-server.ts");

function serverConfig(mode: "typescript" | "volar"): ServerConfig {
	return {
		command: process.execPath,
		args: [fixturePath, mode],
		fileTypes: mode === "volar" ? [".vue"] : [".ts"],
		rootMarkers: [],
	};
}

afterEach(async () => {
	await shutdownAll();
});

describe("Volar tsserver bridge", () => {
	it("forwards tsserver requests through a compatible TypeScript client", async () => {
		const tempDir = TempDir.createSync("@omp-volar-bridge-");
		try {
			await getOrCreateClient(serverConfig("typescript"), tempDir.path(), 1_000);
			const volar = await getOrCreateClient(serverConfig("volar"), tempDir.path(), 1_000);

			const result = await sendRequest(
				volar,
				"textDocument/documentSymbol",
				{ textDocument: { uri: "file:///workspace/App.vue" } },
				undefined,
				1_000,
			);

			expect(result).toEqual({
				command: "typescript.tsserverRequest",
				arguments: ["_vue:projectInfo", { file: "/workspace/App.vue" }],
			});
		} finally {
			tempDir.removeSync();
		}
	});

	it("answers Volar with null when no compatible TypeScript client is live", async () => {
		const tempDir = TempDir.createSync("@omp-volar-no-tsserver-");
		try {
			const volar = await getOrCreateClient(serverConfig("volar"), tempDir.path(), 1_000);

			const result = await sendRequest(
				volar,
				"textDocument/documentSymbol",
				{ textDocument: { uri: "file:///workspace/App.vue" } },
				undefined,
				1_000,
			);

			expect(result).toBeNull();
		} finally {
			tempDir.removeSync();
		}
	});
});
