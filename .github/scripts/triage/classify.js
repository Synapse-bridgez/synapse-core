'use strict';

/**
 * Rules-based triage classifier for newly opened issues.
 *
 * Design goals (see issue #1364):
 *  - Transparent, rules-based classification (path + keyword matching), no ML.
 *  - Category labels match this project's actual subsystem breakdown.
 *  - Priority labeling stays conservative and clearly bot-attributed.
 *  - Ambiguous issues are flagged for human triage, never auto-closed/resolved.
 */

// Category rules, ordered by specificity. The first rule whose path or keyword
// patterns match wins, so more specific subsystems are listed first.
const CATEGORY_RULES = [
  {
    label: 'area: security',
    paths: [/^src\/middleware\/auth/i, /^src\/security\//i, /auth/i, /security/i],
    keywords: [/\bsecurity\b/i, /\bvulnerabilit/i, /\bcve-\d+/i, /\bauth(entication|orization)?\b/i, /\bexploit\b/i],
  },
  {
    label: 'area: alerting',
    paths: [/^src\/alert/i, /alert/i, /notif/i],
    keywords: [/\balert(ing|s)?\b/i, /\bnotif(y|ication)/i, /\bwebhook\b/i, /\bpagerduty\b/i],
  },
  {
    label: 'area: workflows/ci',
    paths: [/^\.github\/workflows\//i, /^\.github\/actions\//i, /^\.github\/scripts\//i],
    keywords: [/\bci\b/i, /\bgithub actions?\b/i, /\bworkflow(s)?\b/i, /\bpipeline\b/i, /\bcodeowners\b/i],
  },
  {
    label: 'area: sdk',
    paths: [/^sdks\//i, /^sdk\//i],
    keywords: [/\bsdk\b/i, /\bclient library\b/i, /\brust sdk\b/i, /\bpython sdk\b/i, /\btypescript sdk\b/i],
  },
  {
    label: 'area: cli',
    paths: [/^src\/cli\//i, /^cli\//i, /^bin\//i],
    keywords: [/\bcli\b/i, /\bcommand[- ]line\b/i, /\bsubcommand\b/i, /\bflag(s)?\b/i],
  },
  {
    label: 'area: api',
    paths: [/^src\/handlers\//i, /^src\/routes\//i, /^src\/api\//i],
    keywords: [/\bapi\b/i, /\bendpoint(s)?\b/i, /\brest\b/i, /\bgraphql\b/i, /\bhandler(s)?\b/i],
  },
  {
    label: 'area: docs',
    paths: [/^docs?\//i, /\.md$/i, /^CONTRIBUTING\.md$/i, /^README\.md$/i],
    keywords: [/\bdocumentation\b/i, /\bdocs?\b/i, /\breadme\b/i, /\btypo\b/i],
  },
];

// Conservative priority rules. Priority labels are only applied when the issue
// text contains explicit, unambiguous signals. They are intentionally narrow so
// the bot never pressures maintainers with implied urgency.
const PRIORITY_RULES = [
  {
    label: 'priority: high',
    keywords: [/\bdata loss\b/i, /\bsecurity (vulnerability|breach)\b/i, /\bproduction (outage|down)\b/i, /\bregression\b/i],
  },
  {
    label: 'priority: low',
    keywords: [/\bnice[- ]to[- ]have\b/i, /\bminor\b/i, /\bcosmetic\b/i, /\btypo\b/i],
  },
];

const FLAG_LABEL = 'needs-triage';
const BOT_ATTRIBUTION = 'triaged-by-bot';

function collectText(issue) {
  const title = issue && issue.title ? String(issue.title) : '';
  const body = issue && issue.body ? String(issue.body) : '';
  return `${title}\n${body}`;
}

function extractReferencedPaths(text) {
  const paths = new Set();
  // Match path-like tokens such as src/handlers/v1/mod.rs or .github/workflows/ci.yml
  const re = /(?:^|[\s`'"(\[])((?:\.?[\w.-]+\/)+[\w.-]+)/g;
  let match;
  while ((match = re.exec(text)) !== null) {
    paths.add(match[1]);
  }
  return Array.from(paths);
}

function matchesAny(patterns, value) {
  return patterns.some((pattern) => pattern.test(value));
}

function classifyCategory(text, paths) {
  for (const rule of CATEGORY_RULES) {
    const pathHit = paths.some((p) => matchesAny(rule.paths, p));
    const keywordHit = matchesAny(rule.keywords, text);
    if (pathHit || keywordHit) {
      return rule.label;
    }
  }
  return null;
}

function classifyPriority(text) {
  for (const rule of PRIORITY_RULES) {
    if (matchesAny(rule.keywords, text)) {
      return rule.label;
    }
  }
  return null;
}

/**
 * Classify an issue into labels.
 *
 * @param {{title?: string, body?: string}} issue
 * @returns {{labels: string[], flagged: boolean, reason: string}}
 */
function classifyIssue(issue) {
  const text = collectText(issue);
  const paths = extractReferencedPaths(text);

  const labels = [BOT_ATTRIBUTION];
  const category = classifyCategory(text, paths);

  if (!category) {
    // Cannot confidently categorize: flag for human triage, do not guess.
    labels.push(FLAG_LABEL);
    return {
      labels,
      flagged: true,
      reason: 'No category rule matched; flagged for human triage.',
    };
  }

  labels.push(category);

  const priority = classifyPriority(text);
  if (priority) {
    labels.push(priority);
  }

  return {
    labels,
    flagged: false,
    reason: `Matched category ${category}${priority ? ` with ${priority}` : ''}.`,
  };
}

module.exports = {
  CATEGORY_RULES,
  PRIORITY_RULES,
  FLAG_LABEL,
  BOT_ATTRIBUTION,
  extractReferencedPaths,
  classifyCategory,
  classifyPriority,
  classifyIssue,
};
