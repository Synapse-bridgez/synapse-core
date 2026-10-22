# Documentation Translations

Human-written translations of the key onboarding docs live here, one directory per language:

```
docs/i18n/
├── README.md              # this file
├── es/
│   ├── README.md          # translation of ../../README.md
│   ├── CONTRIBUTING.md    # translation of ../../CONTRIBUTING.md
│   └── QUICK_START.md     # translation of ../../docs/QUICK_START.md
└── pt-BR/
    └── ...
```

Only these three source documents are translated:

| Source                  | Translation path                   |
|-------------------------|------------------------------------|
| `README.md`             | `docs/i18n/<lang>/README.md`       |
| `CONTRIBUTING.md`       | `docs/i18n/<lang>/CONTRIBUTING.md` |
| `docs/QUICK_START.md`   | `docs/i18n/<lang>/QUICK_START.md`  |

The English documents remain the source of truth. Translations are never machine-generated, and a translation is not authoritative for policy or security questions: when the two disagree, the English document wins.

## Front matter

Every translated file must start with front matter that records the source it was translated from:

```markdown
---
source: docs/QUICK_START.md
source_commit: <full SHA from git log>
---
```

| Key              | Meaning                                                                                      |
|------------------|----------------------------------------------------------------------------------------------|
| `source`         | Source document, relative to the repo root. One of the three paths in the table above.        |
| `source_commit`  | Full SHA of the last commit that changed `source` at the time the translation was made.       |

Get the value with:

```bash
git log -1 --format=%H -- docs/QUICK_START.md
```

## Staleness

`scripts/check-i18n-staleness.py` runs in the `i18n-staleness` workflow on every pull request and on pushes to `main`. For each translation it runs `git diff --quiet <source_commit> HEAD -- <source>`:

| Status    | Meaning                                                                                  |
|-----------|------------------------------------------------------------------------------------------|
| `current` | The source has not changed since `source_commit`.                                        |
| `stale`   | The source has changed since `source_commit`. The translation needs updating.            |
| `invalid` | Front matter is missing or malformed, `source` is not a key doc, or `source_commit` does not exist in history. |

Stale and invalid files are reported as GitHub warning annotations. The check does not block merges, so a source-only change does not stop unrelated PRs; maintainers should treat warnings on the translation as a backlog item and resolve them before each release.

Run it locally with:

```bash
python3 scripts/check-i18n-staleness.py            # report only
python3 scripts/check-i18n-staleness.py --strict   # exit 1 on any stale or invalid file
```

### Restructured source documents

Staleness is tracked per file, not per section. If a source document is restructured (sections moved, renamed, or split), the whole translation is reported as stale and should be re-translated from the new source rather than patched section by section. Section-level tracking is out of scope.

## Updating a stale translation

1. Run `git diff <source_commit> HEAD -- <source>` to see what changed in the English source.
2. Update the translation to match, or re-translate the file if the source was restructured.
3. Set `source_commit` to the commit that last changed the source (the value from `git log -1 --format=%H -- <source>`).
4. Open a PR. The workflow should report `current` for that file.

## Adding a language

1. Open or claim an issue for the language and the documents you will translate.
2. Create `docs/i18n/<lang>/` using a BCP 47 tag (`es`, `pt-BR`, `zh-Hans`).
3. Add the translated files with front matter, then open a PR labelled `docs`.
4. Add a row for the language to the list below when the first file merges.

## Available languages

| Language | Tag | Files |
|----------|-----|-------|
| _None yet_ | | |
