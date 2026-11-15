import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { type DefaultTreeAdapterMap, parse } from "parse5";

type Node = DefaultTreeAdapterMap["node"];
type Element = DefaultTreeAdapterMap["element"];

function elements(node: Node): Element[] {
	const found: Element[] = "tagName" in node ? [node] : [];
	if ("childNodes" in node)
		for (const child of node.childNodes) found.push(...elements(child));
	return found;
}

function attribute(node: Element, name: string): string | undefined {
	return node.attrs.find(
		(value: { name: string; value: string }): boolean => value.name === name,
	)?.value;
}

function text(node: Node): string {
	if ("value" in node) return node.value;
	return "childNodes" in node ? node.childNodes.map(text).join("") : "";
}

await test("browser call markup gives every input an explicit visible label and keeps errors and status available", async (): Promise<void> => {
	const source = await readFile(
		new URL("../index.html", import.meta.url),
		"utf8",
	);
	const nodes = elements(parse(source));
	const startup = nodes.find(
		(node: Element): boolean => attribute(node, "id") === "startup-help",
	);
	assert.ok(
		startup,
		"Disabled startup controls require a visible recovery message.",
	);
	assert.equal(attribute(startup, "hidden"), undefined);
	assert.match(text(startup), /reload the page/i);
	assert.match(text(startup), /enable JavaScript/i);
	assert.equal(
		nodes.filter((node: Element): boolean => node.tagName === "main").length,
		1,
	);
	assert.equal(
		nodes
			.filter((node: Element): boolean => node.tagName === "h1")
			.map(text)
			.join(""),
		"Browser calls",
	);
	const inputs = nodes.filter((node: Element): boolean =>
		["input", "textarea", "select"].includes(node.tagName),
	);
	assert.equal(inputs.length, 6);
	for (const input of inputs) {
		const id = attribute(input, "id");
		assert.ok(id);
		const labels = nodes.filter(
			(node: Element): boolean =>
				node.tagName === "label" && attribute(node, "for") === id,
		);
		assert.equal(labels.length, 1);
		assert.ok(
			labels.every((node: Element): boolean => text(node).trim().length > 0),
		);
	}
	assert.ok(
		nodes.some(
			(node: Element): boolean =>
				node.tagName === "input" &&
				attribute(node, "type") === "password" &&
				attribute(node, "autocomplete") === "current-password",
		),
	);
	assert.equal(
		nodes.filter(
			(node: Element): boolean => attribute(node, "role") === "status",
		).length,
		2,
	);
	assert.ok(
		nodes.some((node: Element): boolean => attribute(node, "role") === "alert"),
	);
	for (const form of nodes.filter(
		(node: Element): boolean => node.tagName === "form",
	))
		assert.equal(
			elements(form).filter(
				(node: Element): boolean => attribute(node, "role") === "alert",
			).length,
			1,
			"Each form must own its recovery message.",
		);
	assert.ok(
		nodes.some(
			(node: Element): boolean =>
				node.tagName === "noscript" && text(node).includes("JavaScript"),
		),
	);
	assert.ok(
		nodes.some(
			(node: Element): boolean =>
				node.tagName === "audio" &&
				attribute(node, "controls") !== undefined &&
				Boolean(attribute(node, "aria-label")),
		),
	);
	for (const button of nodes.filter(
		(node: Element): boolean => node.tagName === "button",
	)) {
		assert.ok(text(button).trim());
		assert.ok(
			elements(button).some(
				(node: Element): boolean =>
					node.tagName === "svg" && attribute(node, "aria-hidden") === "true",
			),
		);
	}
});
