import { afterEach, describe, expect, it } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import InstallWalkthrough from '../components/InstallWalkthrough.svelte';
import { AUDIENCES, hasPlaceholder, usedPrerequisites } from './audiences';

/**
 * Why: `audiences.test.ts` proves the data is right. It cannot prove the page
 * SHOWS it — a panel wired to the wrong audience, a command dropped by a
 * mis-keyed `{#each}`, or a copy button with no accessible name would all pass
 * there and ship. #5110 asks for the rendered form to be asserted, because a
 * command a reader cannot see is as useless as one that is wrong.
 *
 * What: mounts the walkthrough into jsdom and reads the DOM back — one tab and
 * one panel per audience, every command inside ITS OWN panel, every copy
 * button named, no placeholder without its instruction, and the App Data
 * audience free of the wider permission's name. The keyboard case is here for
 * the same reason: a picker that only responds to a mouse locks out the
 * readers most likely to be pasting these commands into a terminal.
 *
 * Test: this file.
 */

let mounted: Record<string, unknown> | undefined;
let target: HTMLElement | undefined;

function render(): HTMLElement {
	target = document.createElement('div');
	document.body.appendChild(target);
	mounted = mount(InstallWalkthrough, { target });
	flushSync();
	return target;
}

afterEach(() => {
	if (mounted) unmount(mounted);
	target?.remove();
	mounted = undefined;
	target = undefined;
});

/** Collapsed text, so a line break in the markup cannot fail a match. */
function text(node: Element): string {
	return (node.textContent ?? '').replace(/\s+/g, ' ');
}

describe('every audience reaches the page', () => {
	it('renders a tab and a panel for all nine', () => {
		const root = render();
		const tabs = root.querySelectorAll('[role="tab"]');
		expect(tabs.length).toBe(AUDIENCES.length);
		for (const audience of AUDIENCES) {
			const tab = root.querySelector(`#tab-${audience.id}`);
			const panel = root.querySelector(`#panel-${audience.id}`);
			expect(tab, `${audience.id} tab`).not.toBeNull();
			expect(panel, `${audience.id} panel`).not.toBeNull();
			expect(text(tab!)).toContain(audience.label);
			expect(text(panel!)).toContain(audience.lede);
		}
	});

	it('shows one audience at a time, and the picker changes which', () => {
		const root = render();
		const hidden = () =>
			AUDIENCES.filter((a) => root.querySelector(`#panel-${a.id}`)!.hasAttribute('hidden'));

		expect(hidden().length).toBe(AUDIENCES.length - 1);
		expect(hidden().map((a) => a.id)).not.toContain(AUDIENCES[0].id);

		const third = root.querySelector<HTMLButtonElement>(`#tab-${AUDIENCES[2].id}`)!;
		third.click();
		flushSync();
		expect(hidden().length).toBe(AUDIENCES.length - 1);
		expect(hidden().map((a) => a.id)).not.toContain(AUDIENCES[2].id);
		expect(third.getAttribute('aria-selected')).toBe('true');
	});
});

describe('every command in the data module is rendered under its own audience', () => {
	it('prints each step command inside that audience’s panel', () => {
		const root = render();
		for (const audience of AUDIENCES) {
			const panel = root.querySelector(`#panel-${audience.id}`)!;
			const rendered = text(panel);
			for (const step of audience.steps) {
				for (const block of step.commands) {
					// The `<pre>` keeps the newlines; comparing collapsed text on
					// both sides makes a multi-line command match line for line.
					const expected = block.command.replace(/\s+/g, ' ');
					expect(rendered, `${audience.id} → ${block.label}`).toContain(expected);
				}
			}
		}
	});

	it('prints every shared prerequisite command exactly once, above the picker', () => {
		const root = render();
		for (const prereq of usedPrerequisites()) {
			const card = root.querySelector(`#prereq-${prereq.id}`);
			expect(card, prereq.id).not.toBeNull();
			for (const block of prereq.commands) {
				const expected = block.command.replace(/\s+/g, ' ');
				expect(text(card!), `${prereq.id} → ${block.label}`).toContain(expected);
				// One card, one copy of the command: a prerequisite repeated per
				// audience is the duplication this page is built to avoid.
				const everywhere = [...root.querySelectorAll('pre')].filter(
					(pre) => text(pre) === expected
				);
				expect(everywhere.length, `${prereq.id} → ${block.label}`).toBe(1);
			}
		}
	});
});

describe('accessibility of the picker and the copy buttons', () => {
	it('names every copy button', () => {
		const root = render();
		const buttons = [...root.querySelectorAll('button')].filter(
			(button) => button.getAttribute('role') !== 'tab'
		);
		expect(buttons.length).toBeGreaterThan(0);
		for (const button of buttons) {
			expect(button.getAttribute('aria-label')?.length ?? 0).toBeGreaterThan(0);
		}
	});

	it('wires the tablist to its panels with a roving tabindex', () => {
		const root = render();
		const list = root.querySelector('[role="tablist"]')!;
		expect(list.getAttribute('aria-label')).toBeTruthy();
		for (const audience of AUDIENCES) {
			const tab = root.querySelector(`#tab-${audience.id}`)!;
			const panel = root.querySelector(`#panel-${audience.id}`)!;
			expect(tab.getAttribute('aria-controls')).toBe(panel.id);
			expect(panel.getAttribute('aria-labelledby')).toBe(tab.id);
			expect(panel.getAttribute('role')).toBe('tabpanel');
		}
		const focusable = [...root.querySelectorAll('[role="tab"]')].filter(
			(tab) => tab.getAttribute('tabindex') === '0'
		);
		expect(focusable.length, 'exactly one tab is in the tab order').toBe(1);
	});

	it('moves the selection with the arrow keys and Home/End', () => {
		const root = render();
		const selected = () => root.querySelector('[role="tab"][aria-selected="true"]')!;
		const selectedId = () => selected().id.replace('tab-', '');

		// Dispatched on the focused tab, not on the tablist: the handler lives on
		// each tab, which is where the ARIA pattern puts it and the only element
		// in the picker that is ever focused.
		const press = (key: string) =>
			selected().dispatchEvent(
				new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true })
			);

		expect(selectedId()).toBe(AUDIENCES[0].id);
		press('ArrowRight');
		flushSync();
		expect(selectedId()).toBe(AUDIENCES[1].id);
		press('End');
		flushSync();
		expect(selectedId()).toBe(AUDIENCES[AUDIENCES.length - 1].id);
		press('ArrowRight');
		flushSync();
		expect(selectedId(), 'wraps past the last tab').toBe(AUDIENCES[0].id);
		press('ArrowLeft');
		flushSync();
		expect(selectedId(), 'wraps before the first tab').toBe(AUDIENCES[AUDIENCES.length - 1].id);
		press('Home');
		flushSync();
		expect(selectedId()).toBe(AUDIENCES[0].id);
	});
});

describe('what the rendered page must never do', () => {
	it('renders no placeholder without the line that replaces it', () => {
		const root = render();
		const blocks = [...root.querySelectorAll('pre')].filter((pre) => hasPlaceholder(text(pre)));
		expect(blocks.length, 'the provider-key exports print one').toBeGreaterThan(0);
		for (const pre of blocks) {
			// CommandBlock.svelte: `<pre>` sits in a wrapper whose sibling `<p>`
			// carries the instruction.
			const wrapper = pre.parentElement!.parentElement!;
			const note = wrapper.querySelector('p');
			expect(note, text(pre)).not.toBeNull();
			for (const placeholder of text(pre).match(/<[^<>\s]+>/g) ?? []) {
				expect(text(note!), `${text(pre)} → ${placeholder}`).toContain(placeholder);
			}
		}
	});

	/**
	 * The security invariant, asserted on the DOM rather than on the data: `tm`
	 * needs the App Data category and must never be offered the disk-wide one.
	 * Scoped to the panel, because the trusty-search panel on the same page
	 * legitimately names it.
	 */
	it('never names Full Disk Access inside the trusty-mpm panel', () => {
		const root = render();
		const panel = root.querySelector('#panel-trusty-mpm')!;
		expect(text(panel)).not.toContain('Full Disk Access');
		expect(text(panel)).toContain('App Data');
		expect(text(root.querySelector('#panel-trusty-agents')!)).not.toContain('Full Disk Access');
		expect(text(root.querySelector('#panel-trusty-search')!)).toContain('Full Disk Access');
	});
});
