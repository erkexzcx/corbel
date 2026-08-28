---
name: release-notes
description: Write corbel's GitHub release notes — the shape they take, what earns a line, how a bug is described, and the measurements that have to be in one. Use whenever a release is being cut or notes are asked for, and before writing anything that will be pasted into a GitHub release body.
---

# Release notes

They are read once, on a phone, by someone deciding whether to upgrade. Short, concrete, no ceremony.

## Deliver them in a fenced code block

Always. The reader pastes them straight into the GitHub release body, so they must arrive as literal Markdown source, not as rendered prose. Never explain the notes around the block — the block is the whole answer.

## The shape

```
Changes:

* One line per change.
* Another.
* Fixed N bugs:
    * One line per bug.
    * Another.
```

- `Changes:` then a blank line, then flat `*` bullets.
- **A sub-bullet is indented four spaces and written `    * `.** Never `* * `, which renders as a bullet containing a bullet.
- **HARD RULE: a sub-bullet exists only where there is a real list under a heading bullet** — `Fixed 15 bugs:` and then the bugs. A single sub-bullet is not a list. If a bullet needs a second sentence, that sentence belongs in the same bullet.
- **Never write a "Full Changelog" line.** GitHub's release UI appends it itself, and writing one duplicates it.
- Nothing else. No headings, no "Highlights", no thanks, no emoji.
- Keep a bullet to one or two sentences. If it needs three, it is two bullets or it is a detail nobody needs.

## What earns a line

Ranked by what a user notices:

1. A defect that changed what came off the printer.
2. A new dial, flag or refusal — anything that changes how the tool is invoked.
3. Test fixtures and checks, but only as a single line saying what they cover.
4. Dependency and packaging changes, one line.
5. Documentation, one line, and only if a figure or a claim changed.

Leave out refactors, renames, and anything a user cannot observe.

## How to describe a bug

State the symptom, then the cause, then the measurement. In that order, in one bullet.

> Every loop's wipe was dropped except the last in each region — 86% of retractions vanished on one file, which is where the stringing, the blobs and 12% of extra filament came from.

- **Symptom first.** The reader is scanning for their own problem.
- **Cite the real file.** "on one file", "on a stock 2-wall Benchy", "on a 492-layer funnel". Never "in some cases".
- **Give the number.** A percentage, a count, a millimetre. A bug without a measurement was not investigated properly, and the number is what makes the note believable.
- **Say what it cost the print**, not what the code does now. `Pass::region_head` means nothing to a reader; "every loop before it printed at the layer below" does.

## Rules that are easy to break

- **Never hard-wrap.** One line per bullet, however long. The renderer wraps.
- **Never write that something is unverified, untested, or "should" work.** If it is not measured, do not claim it.
- **Never suggest a backup** before upgrading.
- **Past tense for the defect, present for the fix.** "The visible wall was laid at the plane… it is now written before any raised loop."
- **No version numbers inside the bullets** except when naming which release had the bug — `v3.0.2 says it adds 1.37% and the part loses 8.01%` is exactly right, because it tells the reader what they are running now.

## Before sending

1. Would someone who hit the bug recognise it from the first six words?
2. Does every bug bullet carry a number?
3. Is every sub-bullet part of a real list under a heading bullet? If one stands alone, fold it into its parent.
4. Is there a stray "Full Changelog" line? Delete it.
5. Is it in a code block?
