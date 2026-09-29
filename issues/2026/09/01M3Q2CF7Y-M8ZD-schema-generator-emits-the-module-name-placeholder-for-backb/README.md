---
id: 01M3Q2CF7YM8ZD2FAZ2Z9A0X5T
number: 1
title: Schema generator emits the MODULE name placeholder for backbone-expenses
type: bug
status: todo
priority: p2
reporter: faridlab
lane: backend
created: 2026-09-29T17:11:18Z
updated: 2026-09-29T17:11:18Z
---

Reproduced 2026-09-29 while executing the #667 activation window. Running metaphor schema generate --force inside modules/backbone-expenses (or with the explicit module arg) emits lib.rs with a placeholder name: the doc header reads MODULE Module, the struct becomes pub struct MODULEModule, the builder MODULEModuleBuilder — the emitted tree does not compile (unresolved ExpensesModule). The same invocation inside modules/backbone-accounting with the same installed metaphor-schema binary (21a770c) regenerates cleanly with the correct name, so the resolution is module-specific, not binary-general: the generator pipeline resolves the module name to the literal placeholder for expenses while accounting resolves correctly. Both schema/models/index.model.yaml files declare module: and schema: equivalently and both modules carry schema/hooks/index.hook.yaml. Suspects: the workspace arg resolution threading (the outer metaphor wrapper is a Sep 11 build whose auto-detect may pass a placeholder for some projects) or a soft index-parse failure swallowed by --force leaving a placeholder default. Probe: instrument generators/module.rs generate_lib_rs (schema.schema.name) once, or diff the wrapper invocation between the two dirs. Blocks the expenses quarter of #364 activation; the window proceeds without it.
