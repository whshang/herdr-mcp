---
name: requirements-grilling
description: Interrogate unresolved product or engineering decisions one at a time before implementation. Use when requirements, scope, architecture, UX, acceptance criteria, or trade-offs contain material ambiguity and the user has not explicitly asked to proceed without clarification; skip when the requested behavior and acceptance criteria are already sufficiently determined.
---

Resolve the intended device, project/workspace, continuity/history, and current repository/runtime facts before asking design questions; investigate every fact you can obtain from tools or code instead of asking the user.
Build a decision tree of only the material choices that can change implementation, then ask exactly one currently-unblocked decision at a time.
For every question, give a concise recommended answer and the main reason; make the recommendation decisive enough that the user can usually confirm it directly.
Continue until no material branch remains silently assumed, then state the agreed decisions and proceed only within that resolved scope.
