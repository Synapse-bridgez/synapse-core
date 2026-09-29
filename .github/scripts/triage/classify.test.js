'use strict';

// Tests for the rules-based community triage classifier.
// Run with: node .github/scripts/triage/classify.test.js
//
// These tests exercise the classification rules against fixture issue bodies
// with known expected labels, including intentionally ambiguous cases that
// should be flagged for human review rather than guessed.

const assert = require('assert');
const { classifyIssue, RULES, CONFIDENCE_THRESHOLD } = require('./classify');

let passed = 0;
let failed = 0;

function test(name, fn) {
  try {
    fn();
    passed += 1;
    console.log(`  ok - ${name}`);
  } catch (err) {
    failed += 1;
    console.error(`  FAIL - ${name}`);
    console.error(`    ${err.message}`);
  }
}

function labelsOf(result) {
  return result.labels.slice().sort();
}

console.log('classifyIssue');

// --- Path-based classification -------------------------------------------

test('classifies CLI issues from referenced file paths', () => {
  const result = classifyIssue({
    title: 'crash when running the command',
    body: 'Running `src/cli/main.rs` panics on startup.',
  });
  assert.ok(labelsOf(result).includes('area: cli'), `expected area: cli, got ${labelsOf(result)}`);
  assert.strictEqual(result.flagged, false);
});

test('classifies SDK issues from referenced file paths', () => {
  const result = classifyIssue({
    title: 'Rust SDK type mismatch',
    body: 'The type in `sdks/rust/src/lib.rs` does not match the docs.',
  });
  assert.ok(labelsOf(result).includes('area: sdk'), `expected area: sdk, got ${labelsOf(result)}`);
});

test('classifies workflow/CI issues from referenced file paths', () => {
  const result = classifyIssue({
    title: 'CI job fails',
    body: 'The job in `.github/workflows/ci.yml` fails on main.',
  });
  assert.ok(labelsOf(result).includes('area: ci'), `expected area: ci, got ${labelsOf(result)}`);
});

test('classifies docs issues from referenced file paths', () => {
  const result = classifyIssue({
    title: 'typo in guide',
    body: 'See `docs/getting-started.md` for the typo.',
  });
  assert.ok(labelsOf(result).includes('area: docs'), `expected area: docs, got ${labelsOf(result)}`);
});

// --- Keyword-based classification ----------------------------------------

test('classifies security issues from keywords', () => {
  const result = classifyIssue({
    title: 'Potential vulnerability in auth',
    body: 'This looks like a security issue that could be exploited.',
  });
  assert.ok(labelsOf(result).includes('area: security'), `expected area: security, got ${labelsOf(result)}`);
});

test('classifies alerting issues from keywords', () => {
  const result = classifyIssue({
    title: 'Alert not firing',
    body: 'The alerting rule never triggers a notification.',
  });
  assert.ok(labelsOf(result).includes('area: alerting'), `expected area: alerting, got ${labelsOf(result)}`);
});

// --- Priority labeling is conservative -----------------------------------

test('does not apply a priority label to a routine issue', () => {
  const result = classifyIssue({
    title: 'Add a small docs note',
    body: 'It would be nice to document this in `docs/faq.md`.',
  });
  const priority = result.labels.filter((l) => l.startsWith('priority:'));
  assert.deepStrictEqual(priority, [], `expected no priority label, got ${priority}`);
});

test('applies at most a conservative priority label when clearly indicated', () => {
  const result = classifyIssue({
    title: 'Data loss on save',
    body: 'This causes data loss and corruption for users.',
  });
  const priority = result.labels.filter((l) => l.startsWith('priority:'));
  assert.ok(priority.length <= 1, `expected at most one priority label, got ${priority}`);
  if (priority.length === 1) {
    assert.strictEqual(priority[0], 'priority: high', 'only conservative priority labels are allowed');
  }
});

test('never applies an urgency-implying label beyond the conservative set', () => {
  const allowed = new Set(['priority: high']);
  const result = classifyIssue({
    title: 'URGENT!!! everything is broken',
    body: 'critical emergency, fix immediately, this is a blocker',
  });
  for (const label of result.labels) {
    if (label.startsWith('priority:')) {
      assert.ok(allowed.has(label), `unexpected priority label ${label}`);
    }
  }
});

// --- Flagging ambiguous issues -------------------------------------------

test('flags an issue with no recognizable signal for human triage', () => {
  const result = classifyIssue({
    title: 'Hello',
    body: 'Just saying hi, no idea where this belongs.',
  });
  assert.strictEqual(result.flagged, true, 'expected ambiguous issue to be flagged');
  assert.ok(labelsOf(result).includes('needs-triage'), `expected needs-triage, got ${labelsOf(result)}`);
});

test('flags an issue that matches multiple categories ambiguously', () => {
  const result = classifyIssue({
    title: 'Something is off',
    body: 'Maybe related to `src/cli/main.rs` or `sdks/rust/src/lib.rs`, not sure.',
  });
  assert.strictEqual(result.flagged, true, 'expected ambiguous multi-category issue to be flagged');
  assert.ok(labelsOf(result).includes('needs-triage'), `expected needs-triage, got ${labelsOf(result)}`);
});

test('does not auto-close or auto-resolve any issue', () => {
  const result = classifyIssue({
    title: 'Hello',
    body: 'Just saying hi.',
  });
  assert.strictEqual(result.close, undefined, 'classifier must not request closing');
  assert.strictEqual(result.resolve, undefined, 'classifier must not request resolving');
});

// --- Rule transparency ----------------------------------------------------

test('exposes transparent, tunable rules', () => {
  assert.ok(Array.isArray(RULES), 'RULES must be an array');
  assert.ok(RULES.length > 0, 'RULES must not be empty');
  for (const rule of RULES) {
    assert.ok(rule.label, 'each rule must declare a label');
    assert.ok(Array.isArray(rule.paths), 'each rule must declare path patterns');
    assert.ok(Array.isArray(rule.keywords), 'each rule must declare keywords');
  }
});

test('confidence threshold is a number between 0 and 1', () => {
  assert.strictEqual(typeof CONFIDENCE_THRESHOLD, 'number');
  assert.ok(CONFIDENCE_THRESHOLD > 0 && CONFIDENCE_THRESHOLD <= 1);
});

console.log(`\n${passed} passed, ${failed} failed`);
if (failed > 0) {
  process.exit(1);
}
